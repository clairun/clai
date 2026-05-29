import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams } from 'react-router-dom';
import { workspaceDeleteAgent } from '../api/client';
import WorkspaceSettingsModal from '../components/Settings/WorkspaceSettingsModal';
import WorkspaceTaskTranscriptPanel from '../components/WorkspaceTaskTranscriptPanel';
import WorkspaceFilePreviewPanel from '../components/WorkspaceFilePreviewPanel';
import * as assistantClient from '../assistant/client';
import useAssistantStore from '../assistant/sessionStore';
import AskUserPanel from '../components/AskUserPanel/AskUserPanel';
import ChatMessageList from '../components/AssistantChat/ChatMessageList';
import InlineApprovalCard from '../components/InlineApprovalCard';
import InlinePathGrantCard from '../components/InlinePathGrantCard';
import VirtualizedList from '../components/common/VirtualizedList';
import { useChatManager } from '../contexts/ChatManagerContext';
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts';
import {
  acknowledgeWorkspaceTask,
  getWorkspaceSnapshot,
  runWorkspaceNow,
  setWorkspaceSchedulePaused,
} from '../workspace/client';
import type {
  AssistantMessage,
  AssistantRun,
  ToolInvocation,
  WorkspaceFileEntry,
  WorkspaceSnapshot,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import styles from './Workspace.module.css';

const DEFAULT_WORKSPACE_ID = 'default';
const REFRESH_INTERVAL_MS = 5000;
// Periodic poll skips the session payload (messages/runs/toolCalls are
// kept in sync via the assistant event stream) but still re-walks the
// workspace filesystem so memories/artifacts created by a running agent
// surface without the user having to re-enter the workspace. The
// backend caps the walk at MAX_ENTRY_COUNT (500) entries with a
// skip-list, so the per-tick cost stays bounded.
const LIGHTWEIGHT_SNAPSHOT_OPTIONS = {
  includeSessionPayload: false,
};

type NumericTimestamp = number | bigint | null | undefined;
type ActivePanel = 'agents' | 'tasks' | 'memories' | 'artifacts' | null;
type PreviewEntry = { kind: 'memory' | 'artifact'; entry: WorkspaceFileEntry };
// The per-workspace "view state": which drawer chip is open plus its
// contextual slide-out (open artifact/memory preview, or task transcript).
// Kept per workspaceId so switching workspaces neither leaks the previous
// workspace's open artifact (which would fail to load here) nor forgets what
// was open when you come back.
type WorkspaceUiState = {
  activePanel: ActivePanel;
  previewEntry: PreviewEntry | null;
  viewingTask: WorkspaceTaskResponse | null;
};
const EMPTY_WORKSPACE_UI: WorkspaceUiState = {
  activePanel: null,
  previewEntry: null,
  viewingTask: null,
};
type SettingsSelection =
  | { kind: 'general' }
  | { kind: 'agent'; agentId: string }
  | { kind: 'new-agent' };
type SnapshotOptions = Parameters<typeof getWorkspaceSnapshot>[1];
type ShortcutHandlers = { onToggleChat?: () => void };
type VirtualizedListProps<T> = {
  items: T[];
  itemKey: (item: T, index: number) => string;
  renderItem: (item: T, index: number) => React.ReactNode;
  className?: string;
  estimateSize?: number;
  overscan?: number;
  gap?: number;
};

const WorkspaceVirtualizedList = VirtualizedList as <T>(
  props: VirtualizedListProps<T>
) => React.ReactElement | null;
const useWorkspaceKeyboardShortcuts = useKeyboardShortcuts as unknown as (
  handlers: ShortcutHandlers,
  enabled?: boolean
) => void;

const toNumber = (value: NumericTimestamp): number | null => {
  if (value === null || value === undefined) return null;
  return typeof value === 'bigint' ? Number(value) : value;
};

const errorMessage = (error: unknown, fallback: string): string => {
  if (typeof error === 'string') return error;
  if (error instanceof Error && error.message) return error.message;
  return fallback;
};

const formatTimestamp = (timestamp: NumericTimestamp): string => {
  const value = toNumber(timestamp);
  if (!value) return 'Never';
  return new Date(value).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

const formatRelativeTime = (timestamp: NumericTimestamp): string => {
  const value = toNumber(timestamp);
  if (!value) return 'Never';
  const diffMs = Date.now() - value;
  const diffSec = Math.max(0, Math.floor(diffMs / 1000));
  if (diffSec < 60) return `${diffSec}s ago`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
};

const formatNextRun = (seconds: number | bigint | null | undefined): string | null => {
  const value = toNumber(seconds);
  if (value === null) return null;
  if (value <= 0) return 'Due now';
  if (value < 60) return `In ${value}s`;
  if (value < 3600) return `In ${Math.floor(value / 60)}m`;
  if (value < 86400) return `In ${Math.floor(value / 3600)}h`;
  return `In ${Math.floor(value / 86400)}d`;
};

const formatSchedulePill = (snapshot: WorkspaceSnapshot | null): string | null => {
  if (!snapshot?.scheduleEnabled) return null;
  const kind = snapshot.scheduleKind;
  let cadence: string | null = null;
  if (kind?.type === 'interval' && Number(kind.intervalMinutes) > 0) {
    cadence = `every ${Number(kind.intervalMinutes)}m`;
  } else if (
    kind?.type === 'cron' &&
    typeof kind.expression === 'string' &&
    kind.expression.trim()
  ) {
    cadence = `cron: ${kind.expression.trim()}`;
  }
  if (snapshot.schedulePaused) {
    return cadence ? `Paused · ${cadence}` : 'Paused';
  }
  return cadence ? `Periodic · ${cadence}` : 'Periodic';
};

const getLastRunInfo = (runs: AssistantRun[] | null | undefined): AssistantRun | null => {
  if (!runs || runs.length === 0) return null;
  // runs are sorted newest first from backend
  const last = [...runs].sort(
    (a, b) => (toNumber(b.startedAt) || 0) - (toNumber(a.startedAt) || 0)
  )[0];
  return last ?? null;
};

const RUN_STATUS_LABEL: Partial<Record<AssistantRun['status'], string>> = {
  completed: 'Completed',
  completed_with_warnings: 'Warnings',
  failed: 'Failed',
  running: 'Running',
  queued: 'Queued',
  cancelled: 'Cancelled',
};

const TASK_STATUS_LABEL: Record<string, string> = {
  queued: 'Queued',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  blocked: 'Blocked',
};

const ACTIVE_RUN_STATUSES: AssistantRun['status'][] = ['queued', 'running', 'waiting_for_tool'];

const isTaskAttention = (task: WorkspaceTaskResponse): boolean =>
  (task.status === 'blocked' || task.status === 'failed') &&
  !task.attentionAcknowledgedAt &&
  !task.userResponseAt;

interface WorkspaceAgentsPanelProps {
  workspaceId: string;
  snapshot: WorkspaceSnapshot | null;
  busy: string;
  error: string;
  onOpenEdit: (workspaceAgentId: string) => void;
  onRemove: (workspaceAgentId: string) => void;
}

const WorkspaceAgentsPanel = ({
  workspaceId,
  snapshot,
  busy,
  error,
  onOpenEdit,
  onRemove,
}: WorkspaceAgentsPanelProps) => {
  const assignedAgents = snapshot?.assignedAgents || [];
  const isManageable = snapshot?.kind !== 'agent' && workspaceId !== DEFAULT_WORKSPACE_ID;

  // Manager first (rendered as "Main"), then sub-agents. The manager is
  // always present and not removable; Edit deep-links into the workspace
  // settings modal just like sub-agents.
  const sortedAgents = [...assignedAgents].sort((a, b) => {
    if (a.isDefault === b.isDefault) return 0;
    return a.isDefault ? -1 : 1;
  });

  if (!isManageable && sortedAgents.length === 0) {
    return null;
  }

  return (
    <section className={styles.agentRoster} aria-label="Workspace agents">
      {error && <div className={styles.agentRosterError}>{error}</div>}

      {sortedAgents.length > 0 ? (
        <div className={styles.agentRosterList}>
          {sortedAgents.map((agent) => (
            <div key={agent.id} className={styles.agentRosterItem}>
              <div className={styles.agentRosterIdentity}>
                <div className={styles.agentRosterNameRow}>
                  <span className={styles.agentRosterName}>
                    {agent.isDefault ? 'Main' : agent.displayName}
                  </span>
                </div>
                {agent.agentDescription && (
                  <p className={styles.agentRosterDescription}>{agent.agentDescription}</p>
                )}
              </div>
              {isManageable && (
                <div className={styles.agentRosterActions}>
                  <button
                    type="button"
                    className={styles.agentAction}
                    onClick={() => onOpenEdit(agent.id)}
                    disabled={!!busy}
                  >
                    Edit
                  </button>
                  {!agent.isDefault && (
                    <button
                      type="button"
                      className={styles.agentActionDanger}
                      onClick={() => onRemove(agent.id)}
                      disabled={!!busy}
                    >
                      Remove
                    </button>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      ) : (
        <div className={styles.agentRosterEmpty}>
          The workspace itself is the entry-point agent — its configuration is edited via the gear
          icon next to the workspace title. Agents added here are optional helpers the workspace can
          call as tools.
        </div>
      )}
    </section>
  );
};

interface WorkspaceTasksPanelProps {
  workspaceId: string;
  tasks: WorkspaceTaskResponse[];
  onChanged: () => void | Promise<void>;
  onViewTask?: (task: WorkspaceTaskResponse) => void;
}

const WorkspaceTasksPanel = ({
  workspaceId,
  tasks,
  onChanged,
  onViewTask,
}: WorkspaceTasksPanelProps) => {
  const visibleTasks = tasks || [];
  const [busyTaskId, setBusyTaskId] = useState('');
  const [error, setError] = useState('');

  const handleAcknowledge = useCallback(
    async (taskId: string) => {
      if (busyTaskId) return;
      setBusyTaskId(taskId);
      setError('');
      try {
        await acknowledgeWorkspaceTask(workspaceId, taskId);
        await onChanged();
      } catch (err) {
        setError(errorMessage(err, 'Failed to acknowledge task.'));
      } finally {
        setBusyTaskId('');
      }
    },
    [busyTaskId, onChanged, workspaceId]
  );

  return (
    <section className={styles.taskActivity} aria-label="Workspace task activity">
      <div className={styles.taskActivityHeader}>
        <div className={styles.agentRosterTitleBlock}>
          <h2 className={styles.agentRosterTitle}>Task Activity</h2>
          <span className={styles.agentRosterMeta}>{visibleTasks.length} recent</span>
        </div>
      </div>

      {error && <div className={styles.agentRosterError}>{error}</div>}

      {visibleTasks.length > 0 ? (
        <div className={styles.taskList}>
          {visibleTasks.map((task) => {
            const statusLabel = TASK_STATUS_LABEL[task.status] || task.status;
            const detail = task.error || task.resultSummary || task.instructions;
            const needsAttention = isTaskAttention(task);
            return (
              <div key={task.id} className={styles.taskItem}>
                <div className={styles.taskMain}>
                  <div className={styles.taskTitleRow}>
                    <span className={styles.taskTitle}>{task.title}</span>
                    <span
                      className={`${styles.taskStatus} ${styles[`taskStatus_${task.status}`] || ''}`}
                    >
                      {statusLabel}
                    </span>
                  </div>
                  <div className={styles.taskMeta}>
                    <span>{task.assignedAgentDisplayName}</span>
                    <span className={styles.metricSeparator}>{'\u00B7'}</span>
                    <span>{formatRelativeTime(task.updatedAt)}</span>
                  </div>
                  {detail && <p className={styles.taskSummary}>{detail}</p>}
                  {needsAttention && (
                    <div className={styles.taskActions}>
                      <button
                        type="button"
                        className={styles.taskAction}
                        onClick={() => handleAcknowledge(task.id)}
                        disabled={busyTaskId === task.id}
                      >
                        Mark reviewed
                      </button>
                      {task.sessionId && (
                        <button
                          type="button"
                          className={styles.taskAction}
                          onClick={() => onViewTask?.(task)}
                        >
                          View log
                        </button>
                      )}
                    </div>
                  )}
                  {!needsAttention && task.sessionId && (
                    <div className={styles.taskActions}>
                      <button
                        type="button"
                        className={styles.taskAction}
                        onClick={() => onViewTask?.(task)}
                      >
                        View log
                      </button>
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <div className={styles.agentRosterEmpty}>No delegated tasks yet.</div>
      )}
    </section>
  );
};

interface WorkspaceFileEntryListProps {
  entries: WorkspaceFileEntry[];
  emptyMessage: string;
  onSelect?: (entry: WorkspaceFileEntry) => void;
}

const WorkspaceFileEntryList = ({
  entries,
  emptyMessage,
  onSelect,
}: WorkspaceFileEntryListProps) => {
  const itemKey = useCallback((entry: WorkspaceFileEntry) => entry.path, []);
  const renderEntry = useCallback(
    (entry: WorkspaceFileEntry) => (
      <button type="button" className={styles.drawerListItem} onClick={() => onSelect?.(entry)}>
        <div className={styles.drawerListName}>{entry.name}</div>
        <div className={styles.drawerListMeta}>
          {entry.path}
          {entry.updatedAt ? ` · ${formatTimestamp(entry.updatedAt)}` : ''}
        </div>
      </button>
    ),
    [onSelect]
  );

  if (!entries || entries.length === 0) {
    return <div className={styles.drawerEmpty}>{emptyMessage}</div>;
  }

  return (
    <WorkspaceVirtualizedList
      items={entries}
      itemKey={itemKey}
      renderItem={renderEntry}
      className={styles.drawerVirtualList}
      estimateSize={58}
      overscan={500}
      gap={6}
    />
  );
};

// ── Artifact file-tree browser ────────────────────────────────────────────
// Artifacts arrive as a flat list of { name, path, updatedAt }. For large
// workspaces (hundreds of files) a flat list is hard to navigate, so we
// reconstruct the folder hierarchy from each entry's `path` and render it
// as a collapsible tree with one folder/file per row.

type ArtifactFileNode = {
  kind: 'file';
  name: string;
  path: string;
  depth: number;
  entry: WorkspaceFileEntry;
};

type ArtifactFolderDraft = {
  kind: 'folder';
  name: string;
  path: string;
  depth: number;
  children: Map<string, ArtifactDraftNode>;
};

type ArtifactDraftNode = ArtifactFolderDraft | ArtifactFileNode;

type ArtifactFolderNode = {
  kind: 'folder';
  name: string;
  path: string;
  depth: number;
  children: ArtifactTreeNode[];
  fileCount: number;
};

type ArtifactTreeNode = ArtifactFolderNode | ArtifactFileNode;

const buildArtifactTree = (artifacts: WorkspaceFileEntry[]): ArtifactFolderNode => {
  const root: ArtifactFolderDraft = {
    kind: 'folder',
    name: '',
    path: '',
    depth: -1,
    children: new Map(),
  };
  for (const entry of artifacts) {
    const parts = (entry.path || entry.name || '').split('/').filter(Boolean);
    if (parts.length === 0) continue;
    let node = root;
    for (let i = 0; i < parts.length; i += 1) {
      const part = parts[i]!; // bounded by the loop condition
      const isLeaf = i === parts.length - 1;
      const childPath = node.path ? `${node.path}/${part}` : part;
      let child = node.children.get(part);
      if (!child) {
        child = isLeaf
          ? { kind: 'file', name: part, path: childPath, depth: i, entry }
          : { kind: 'folder', name: part, path: childPath, depth: i, children: new Map() };
        node.children.set(part, child);
      } else if (!isLeaf && child.kind === 'file') {
        // Rare: a segment was registered as a file, but a deeper path now
        // uses it as a folder. Promote to folder so traversal continues.
        child = { kind: 'folder', name: part, path: childPath, depth: i, children: new Map() };
        node.children.set(part, child);
      }
      if (child.kind === 'folder') {
        node = child;
      }
    }
  }

  const finalize = (node: ArtifactDraftNode): ArtifactTreeNode => {
    if (node.kind === 'file') return node;
    const arr = [...node.children.values()];
    arr.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === 'folder' ? -1 : 1;
      return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    });
    const children = arr.map(finalize);
    const fileCount = children.reduce(
      (count, child) => count + (child.kind === 'file' ? 1 : child.fileCount),
      0
    );
    return {
      kind: 'folder',
      name: node.name,
      path: node.path,
      depth: node.depth,
      children,
      fileCount,
    };
  };
  return finalize(root) as ArtifactFolderNode;
};

// When the search box has content, walk the tree once and collect every
// matching file path plus every ancestor folder path. The flatten pass then
// uses this set both as a visibility filter and (since it contains all
// ancestor folders) as the effective "expanded" set — so matches always
// reveal themselves without disturbing the user's manual expansion state.
const computeArtifactMatches = (root: ArtifactFolderNode, query: string): Set<string> | null => {
  if (!query) return null;
  const q = query.toLowerCase();
  const matched = new Set<string>();
  const walk = (node: ArtifactTreeNode): boolean => {
    if (node.kind === 'file') {
      if (node.name.toLowerCase().includes(q) || node.path.toLowerCase().includes(q)) {
        matched.add(node.path);
        const parts = node.path.split('/');
        parts.pop();
        while (parts.length > 0) {
          matched.add(parts.join('/'));
          parts.pop();
        }
        return true;
      }
      return false;
    }
    let any = false;
    for (const child of node.children) {
      if (walk(child)) any = true;
    }
    return any;
  };
  for (const child of root.children) walk(child);
  return matched;
};

const flattenArtifactTree = (
  root: ArtifactFolderNode,
  expanded: Set<string>,
  matches: Set<string> | null
): ArtifactTreeNode[] => {
  const out: ArtifactTreeNode[] = [];
  const walk = (node: ArtifactFolderNode) => {
    for (const child of node.children) {
      if (matches && !matches.has(child.path)) continue;
      out.push(child);
      if (child.kind === 'folder' && expanded.has(child.path)) {
        walk(child);
      }
    }
  };
  walk(root);
  return out;
};

const HighlightedText = ({ text, query }: { text: string; query: string }): React.ReactNode => {
  if (!query) return text;
  const idx = text.toLowerCase().indexOf(query.toLowerCase());
  if (idx === -1) return text;
  return (
    <>
      {text.slice(0, idx)}
      <mark className={styles.fileTreeMark}>{text.slice(idx, idx + query.length)}</mark>
      {text.slice(idx + query.length)}
    </>
  );
};

const ChevronIcon = ({ open }: { open: boolean }) => (
  <svg
    className={`${styles.fileTreeChevron} ${open ? styles.fileTreeChevronOpen : ''}`}
    width="10"
    height="10"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <polyline points="9 6 15 12 9 18" />
  </svg>
);

const FolderGlyph = () => (
  <svg
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  </svg>
);

const FileGlyph = () => (
  <svg
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
  </svg>
);

interface ArtifactTreeRowProps {
  node: ArtifactTreeNode;
  isExpanded: boolean;
  query: string;
  onToggle: (path: string) => void;
  onSelect?: (entry: WorkspaceFileEntry) => void;
}

const ArtifactTreeRow = ({ node, isExpanded, query, onToggle, onSelect }: ArtifactTreeRowProps) => {
  const isFolder = node.kind === 'folder';
  const handleClick = () => {
    if (isFolder) onToggle(node.path);
    else onSelect?.(node.entry);
  };
  return (
    <button
      type="button"
      className={`${styles.fileTreeRow} ${isFolder ? styles.fileTreeRowFolder : styles.fileTreeRowFile}`}
      style={{ paddingInlineStart: 8 + node.depth * 14 }}
      onClick={handleClick}
      title={node.path}
      aria-expanded={isFolder ? isExpanded : undefined}
    >
      <span className={styles.fileTreeChevronSlot}>
        {isFolder && <ChevronIcon open={isExpanded} />}
      </span>
      <span className={styles.fileTreeIcon}>{isFolder ? <FolderGlyph /> : <FileGlyph />}</span>
      <span className={styles.fileTreeName}>
        <HighlightedText text={node.name} query={query} />
      </span>
      <span className={styles.fileTreeMeta}>
        {isFolder
          ? node.fileCount
          : node.entry?.updatedAt
            ? formatTimestamp(node.entry.updatedAt)
            : ''}
      </span>
    </button>
  );
};

interface ArtifactsListProps {
  artifacts: WorkspaceFileEntry[];
  onSelect?: (entry: WorkspaceFileEntry) => void;
}

const ArtifactsList = ({ artifacts, onSelect }: ArtifactsListProps) => {
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const initializedRef = useRef(false);

  const tree = useMemo(() => buildArtifactTree(artifacts || []), [artifacts]);
  const total = (artifacts || []).length;

  // On first non-empty load, auto-expand the sole top-level folder so the
  // user doesn't have to click once to see anything — common case for
  // repo-rooted artifacts like `work/<repo>/...`.
  useEffect(() => {
    if (initializedRef.current) return;
    if (tree.children.length === 0) return;
    initializedRef.current = true;
    if (tree.children.length === 1 && tree.children[0]!.kind === 'folder') {
      setExpanded(new Set([tree.children[0]!.path]));
    }
  }, [tree]);

  const trimmedQuery = query.trim();
  const matches = useMemo(() => computeArtifactMatches(tree, trimmedQuery), [tree, trimmedQuery]);

  const visibleNodes = useMemo(
    () => flattenArtifactTree(tree, matches || expanded, matches),
    [tree, expanded, matches]
  );

  const handleToggle = useCallback((path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  const itemKey = useCallback((node: ArtifactTreeNode) => node.path, []);
  const renderItem = useCallback(
    (node: ArtifactTreeNode) => (
      <ArtifactTreeRow
        node={node}
        isExpanded={matches ? true : expanded.has(node.path)}
        query={trimmedQuery}
        onToggle={handleToggle}
        onSelect={onSelect}
      />
    ),
    [expanded, matches, trimmedQuery, handleToggle, onSelect]
  );

  return (
    <div className={styles.searchableList}>
      {total > 0 && (
        <input
          type="text"
          className={styles.searchInput}
          value={query}
          onChange={(event: React.ChangeEvent<HTMLInputElement>) => setQuery(event.target.value)}
          placeholder={`Search artifacts (${total})`}
          aria-label="Search artifacts"
        />
      )}
      {total === 0 ? (
        <div className={styles.drawerEmpty}>No artifacts in this workspace yet.</div>
      ) : visibleNodes.length === 0 ? (
        <div className={styles.drawerEmpty}>No artifacts match &quot;{query}&quot;.</div>
      ) : (
        <WorkspaceVirtualizedList
          items={visibleNodes}
          itemKey={itemKey}
          renderItem={renderItem}
          className={styles.drawerVirtualList}
          estimateSize={28}
          overscan={400}
          gap={0}
        />
      )}
    </div>
  );
};

const WorkspaceAttentionBanner = ({ tasks }: { tasks: WorkspaceTaskResponse[] }) => {
  const attentionTasks = (tasks || []).filter(isTaskAttention);

  if (attentionTasks.length === 0) {
    return null;
  }

  const primary = attentionTasks[0]!;
  const statusLabel = TASK_STATUS_LABEL[primary.status] || primary.status;
  const detail = primary.error || primary.resultSummary || primary.instructions;

  return (
    <section className={styles.attentionBanner} aria-label="Workspace attention">
      <div className={styles.attentionBannerHeader}>
        <span className={styles.attentionBannerTitle}>
          {attentionTasks.length} task{attentionTasks.length === 1 ? '' : 's'} need attention
        </span>
        <span className={`${styles.taskStatus} ${styles[`taskStatus_${primary.status}`] || ''}`}>
          {statusLabel}
        </span>
      </div>
      <div className={styles.attentionBannerTask}>{primary.title}</div>
      {detail && <p className={styles.attentionBannerDetail}>{detail}</p>}
    </section>
  );
};

/**
 * Compact workspace header with breadcrumb navigation, status, and inline metrics.
 */
const WorkspaceHeader = ({
  snapshot,
  workspaceId,
  isGenericWorkspace,
  messages,
  memories,
  artifacts,
  activePanel,
  setActivePanel,
  onRunNow,
  onTogglePause,
  onStop,
  activeRunId,
  hasActiveRun,
  runNowBusy,
  pauseBusy,
  stopBusy,
}: {
  snapshot: WorkspaceSnapshot | null;
  workspaceId: string;
  isGenericWorkspace: boolean;
  messages: AssistantMessage[];
  memories: WorkspaceFileEntry[];
  artifacts: WorkspaceFileEntry[];
  activePanel: ActivePanel;
  setActivePanel: React.Dispatch<React.SetStateAction<ActivePanel>>;
  onRunNow: () => void | Promise<void>;
  onTogglePause: (paused: boolean) => void | Promise<void>;
  onStop: (runId: string | null) => void | Promise<void>;
  activeRunId: string | null;
  hasActiveRun: boolean;
  runNowBusy: boolean;
  pauseBusy: boolean;
  stopBusy: boolean;
}) => {
  const isAgent = snapshot?.kind === 'agent';
  const lastRun = getLastRunInfo(snapshot?.runs);
  const nextRunText = formatNextRun(snapshot?.nextRunInSeconds);
  const schedulePillText = formatSchedulePill(snapshot);
  const scheduleEnabled = !!snapshot?.scheduleEnabled;
  const schedulePaused = !!snapshot?.schedulePaused;
  // Active = a scheduled task is running, or any non-terminal task is in
  // flight on this workspace. Matches Fleet's "isProcessing" check so the
  // Run-now button correctly disables while a run is mid-flight.
  const hasRunningTask = (snapshot?.tasks || []).some(
    (task) => task.status === 'running' || task.status === 'queued'
  );
  // Manager is invisible to the user — exclude it from the headline count so
  // the chip and the drawer (which already filters !isDefault) agree.
  // Count includes the main (default) agent — the manager is now a
  // first-class entry in the workspace's agent list.
  const assignedAgentCount = (snapshot?.assignedAgents || []).length;
  const taskCount = snapshot?.tasks?.length || 0;
  const activeTaskCount = (snapshot?.tasks || []).filter(
    (task) => task.status === 'running' || task.status === 'queued'
  ).length;

  // Click a counter to open its panel; click again (or click another) to switch.
  // null = no panel open, chat takes the full content area.
  const togglePanel = (panel: ActivePanel) => {
    setActivePanel((current) => (current === panel ? null : panel));
  };

  const renderCounter = (
    panel: ActivePanel,
    count: number,
    label: string,
    clickable = true,
    activeCount = 0
  ) => {
    const isActive = activePanel === panel;
    if (!clickable) {
      return (
        <span className={styles.metric}>
          {count} {label}
        </span>
      );
    }
    return (
      <button
        type="button"
        className={`${styles.metricButton} ${isActive ? styles.metricButtonActive : ''}`}
        onClick={() => togglePanel(panel)}
        title={activeCount > 0 ? `${activeCount} ${label} in flight` : `Toggle ${label} panel`}
      >
        {activeCount > 0 && (
          <span
            className={`${styles.statusDot} ${styles.status_running} ${styles.metricLeadingDot}`}
            aria-hidden="true"
          />
        )}
        {count} {label}
        {activeCount > 0 && (
          <span className={styles.metricActiveSuffix}> · {activeCount} active</span>
        )}
      </button>
    );
  };

  return (
    <div className={styles.header}>
      <div className={styles.headerLeft}>
        <h1 className={styles.title}>
          {snapshot?.title || (isGenericWorkspace ? 'Workspace' : workspaceId)}
        </h1>
        {scheduleEnabled && (
          <span
            className={`${styles.schedulePill} ${schedulePaused ? styles.schedulePillPaused : styles.schedulePillActive}`}
            title={schedulePillText || undefined}
          >
            {schedulePillText}
          </span>
        )}
        {hasActiveRun && (
          <button
            type="button"
            className={styles.stopBtn}
            onClick={() => onStop?.(activeRunId)}
            disabled={!onStop || !activeRunId || stopBusy}
            title={stopBusy ? 'Stopping…' : 'Stop current run'}
            aria-label="Stop current run"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <rect x="6" y="6" width="12" height="12" rx="2" />
            </svg>
          </button>
        )}
        {scheduleEnabled && !schedulePaused && !hasActiveRun && (
          <button
            type="button"
            className={styles.runNowBtn}
            onClick={onRunNow}
            disabled={!onRunNow || hasRunningTask || runNowBusy}
            title={hasRunningTask ? 'Already running' : runNowBusy ? 'Starting…' : 'Run now'}
            aria-label="Run now"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M8 5v14l11-7z" />
            </svg>
          </button>
        )}
        {scheduleEnabled && !schedulePaused && (
          <button
            type="button"
            className={styles.pauseBtn}
            onClick={() => onTogglePause?.(true)}
            disabled={!onTogglePause || pauseBusy}
            title={
              pauseBusy
                ? 'Updating…'
                : hasActiveRun
                  ? 'Pause schedule (current run will keep going — use Stop to cancel it)'
                  : hasRunningTask
                    ? 'Pause schedule (current run will finish)'
                    : 'Pause schedule'
            }
            aria-label="Pause schedule"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <rect x="6" y="5" width="4" height="14" rx="1" />
              <rect x="14" y="5" width="4" height="14" rx="1" />
            </svg>
          </button>
        )}
        {scheduleEnabled && schedulePaused && (
          <button
            type="button"
            className={styles.resumeBtn}
            onClick={() => onTogglePause?.(false)}
            disabled={!onTogglePause || pauseBusy}
            title={pauseBusy ? 'Updating…' : 'Resume schedule'}
            aria-label="Resume schedule"
          >
            Resume
          </button>
        )}
      </div>
      <div className={styles.headerRight}>
        {isAgent && lastRun && (
          <>
            <span className={`${styles.statusDot} ${styles[`status_${lastRun.status}`]}`} />
            <span className={styles.metric}>
              {RUN_STATUS_LABEL[lastRun.status] || lastRun.status}
            </span>
            <span className={styles.metricSeparator}>{'\u00B7'}</span>
            <span className={styles.metric}>
              Last: {formatRelativeTime(lastRun.completedAt || lastRun.startedAt)}
            </span>
            {nextRunText && (
              <>
                <span className={styles.metricSeparator}>{'\u00B7'}</span>
                <span className={styles.metric}>Next: {nextRunText}</span>
              </>
            )}
            <span className={styles.metricSeparator}>{'\u00B7'}</span>
          </>
        )}
        {renderCounter(null, messages.length, 'msgs', false)}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('agents', assignedAgentCount, 'agents')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('tasks', taskCount, 'tasks', true, activeTaskCount)}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('memories', memories.length, 'memories')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('artifacts', artifacts.length, 'artifacts')}
      </div>
    </div>
  );
};

// Chat is the workspace's primary surface. Memories, artifacts, tasks, and
// member agents live in the drawer (toggled from the header counters) and
// open in modals when inspected — the chat is never hidden.
interface ChatFirstLayoutProps {
  sessionId: string | null;
  workspaceId: string;
  messages: AssistantMessage[];
  toolCalls: ToolInvocation[];
  streamingText: Record<string, string>;
  isStreaming: boolean;
}

const ChatFirstLayout = ({
  sessionId,
  workspaceId,
  messages,
  toolCalls,
  streamingText,
  isStreaming,
}: ChatFirstLayoutProps) => (
  <div className={styles.chatFirstContent}>
    {messages.length > 0 ? (
      <>
        <ChatMessageList
          messages={messages}
          toolCalls={toolCalls}
          streamingText={streamingText}
          isStreaming={isStreaming}
        />
        <AskUserPanel sessionId={sessionId} />
        <InlineApprovalCard workspaceId={workspaceId} />
        <InlinePathGrantCard workspaceId={workspaceId} />
      </>
    ) : (
      <div className={styles.chatFirstEmpty}>
        <div className={styles.chatFirstEmptyIcon}>
          <svg
            width="40"
            height="40"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
        </div>
        <p className={styles.chatFirstEmptyTitle}>Start a conversation</p>
        <p className={styles.chatFirstEmptyText}>
          Type a message in the terminal below to begin. This workspace can search the web, create
          documents, and use any attached MCP servers.
        </p>
      </div>
    )}
  </div>
);

const Workspace = () => {
  const params = useParams();
  const { toggleChat } = useChatManager() as { toggleChat: () => void };
  const workspaceId = params.workspaceId || DEFAULT_WORKSPACE_ID;
  const isGenericWorkspace = workspaceId === DEFAULT_WORKSPACE_ID;
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [, setIsLoading] = useState(true);
  const [error, setError] = useState('');
  // The drawer chip that's open plus its contextual slide-out panel. The
  // Workspace component instance is REUSED across workspace→workspace
  // navigations (both routes resolve to the same element), so a plain useState
  // would leak one workspace's open artifact into the next — where it fails to
  // load because the file lives elsewhere. Keying by workspaceId fixes that and
  // restores whatever was open when you return to a workspace.
  //   - activePanel:  null = chat-only; 'agents'|'tasks'|'memories'|'artifacts'
  //   - previewEntry: { kind: 'memory' | 'artifact', entry } — file preview
  //   - viewingTask:  task object — task transcript log
  // Only one slide-out may be open at a time; opening one clears the other.
  const [uiByWorkspace, setUiByWorkspace] = useState<Record<string, WorkspaceUiState>>({});
  const { activePanel, previewEntry, viewingTask } =
    uiByWorkspace[workspaceId] ?? EMPTY_WORKSPACE_UI;

  const patchWorkspaceUi = useCallback(
    (patch: Partial<WorkspaceUiState>) => {
      setUiByWorkspace((prev) => {
        const current = prev[workspaceId] ?? EMPTY_WORKSPACE_UI;
        return { ...prev, [workspaceId]: { ...current, ...patch } };
      });
    },
    [workspaceId]
  );

  // Side panels are contextual to the open drawer chip — switching to a panel
  // that doesn't own the slide-out content clears it. Folding that invariant
  // into the setter keeps it atomic (no stale flash between renders).
  const setActivePanel = useCallback<React.Dispatch<React.SetStateAction<ActivePanel>>>(
    (action) => {
      setUiByWorkspace((prev) => {
        const current = prev[workspaceId] ?? EMPTY_WORKSPACE_UI;
        const next = typeof action === 'function' ? action(current.activePanel) : action;
        return {
          ...prev,
          [workspaceId]: {
            activePanel: next,
            previewEntry: next === 'memories' || next === 'artifacts' ? current.previewEntry : null,
            viewingTask: next === 'tasks' ? current.viewingTask : null,
          },
        };
      });
    },
    [workspaceId]
  );

  const openPreviewEntry = useCallback(
    (next: PreviewEntry) => {
      patchWorkspaceUi({ previewEntry: next, viewingTask: null });
    },
    [patchWorkspaceUi]
  );

  const openTaskTranscript = useCallback(
    (task: WorkspaceTaskResponse) => {
      patchWorkspaceUi({ previewEntry: null, viewingTask: task });
    },
    [patchWorkspaceUi]
  );

  const closePreview = useCallback(() => patchWorkspaceUi({ previewEntry: null }), [patchWorkspaceUi]);
  const closeTaskTranscript = useCallback(
    () => patchWorkspaceUi({ viewingTask: null }),
    [patchWorkspaceUi]
  );

  const isSidePanelOpen = !!previewEntry || !!viewingTask;

  // ── Workspace Settings modal (replaces the legacy AgentFormModal
  //    workspace-mode hack). Selection drives which section/agent the
  //    modal opens to: gear icon -> General, drawer Edit -> agent:<id>,
  //    drawer "+ Add" -> new-agent. ────────────────────────────────────
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSelection, setSettingsSelection] = useState<SettingsSelection>({
    kind: 'general',
  });
  const [agentBusy, setAgentBusy] = useState('');
  const [agentError, setAgentError] = useState('');
  const sessionId = snapshot?.session?.id || null;
  const sessionState = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId] || null : null
  );
  const lastLoadedSessionUpdatedAtRef = useRef<NumericTimestamp>(null);

  // Register Ctrl/Cmd+Shift+C to toggle chat panel — only for agent workspaces.
  // General workspaces embed chat directly in the page.
  useWorkspaceKeyboardShortcuts({
    onToggleChat: () => {
      if (snapshot?.kind === 'agent') {
        toggleChat();
      }
    },
  });

  const loadSnapshot = useCallback(
    async (showSpinner = false, options: SnapshotOptions = null) => {
      if (showSpinner) {
        setIsLoading(true);
      }

      const isLightweight = options !== null;

      try {
        const nextSnapshot = await getWorkspaceSnapshot(workspaceId, options);
        setSnapshot((current) => {
          if (!isLightweight || !current) {
            return nextSnapshot;
          }

          // Lightweight refresh: the backend skipped the session payload
          // (messages/runs/toolCalls live in the assistant event store),
          // so preserve those from the prior snapshot. Memories and
          // artifacts ARE re-fetched so writes made by a running agent
          // appear without the user having to re-enter the workspace.
          return {
            ...nextSnapshot,
            messages: current.messages || [],
            toolCalls: current.toolCalls || [],
          };
        });
        setError('');

        if (nextSnapshot?.session) {
          const store = useAssistantStore.getState();
          store.setActiveSessionForTab(`workspace:${workspaceId}`, nextSnapshot.session.id);

          const existingSession = store.sessions[nextSnapshot.session.id];
          const needsInitialHydration = !existingSession;
          const hasUnloadedUpdate =
            nextSnapshot.session.updatedAt &&
            lastLoadedSessionUpdatedAtRef.current !== nextSnapshot.session.updatedAt;
          const shouldHydrateSession =
            !isLightweight ||
            needsInitialHydration ||
            (hasUnloadedUpdate && !existingSession?.isStreaming);

          if (shouldHydrateSession) {
            let messages: AssistantMessage[];
            let runs: AssistantRun[];
            let toolCalls: ToolInvocation[];
            if (isLightweight) {
              [messages, runs, toolCalls] = await Promise.all([
                assistantClient.loadSessionMessages(nextSnapshot.session.id),
                assistantClient.listRuns(nextSnapshot.session.id),
                assistantClient.listToolCalls(nextSnapshot.session.id),
              ]);
            } else {
              messages = nextSnapshot.messages || [];
              runs = nextSnapshot.runs || [];
              toolCalls = nextSnapshot.toolCalls || [];
            }

            store.loadSessionData(
              nextSnapshot.session.id,
              nextSnapshot.session,
              messages,
              runs,
              toolCalls
            );
            lastLoadedSessionUpdatedAtRef.current = nextSnapshot.session.updatedAt || null;
          }
        }
      } catch (err) {
        setError(errorMessage(err, 'Failed to load workspace.'));
      } finally {
        setIsLoading(false);
      }
    },
    [workspaceId]
  );

  // ── Workspace Settings modal openers ───────────────────────────────────
  const openSettings = useCallback((selection?: SettingsSelection | null) => {
    setSettingsSelection(selection || { kind: 'general' });
    setSettingsOpen(true);
    setAgentError('');
  }, []);

  const openAgentEdit = useCallback(
    (workspaceAgentId: string) => {
      openSettings({ kind: 'agent', agentId: workspaceAgentId });
    },
    [openSettings]
  );

  const openMemberCreate = useCallback(() => {
    openSettings({ kind: 'new-agent' });
  }, [openSettings]);

  const handleSettingsClose = useCallback(() => {
    setSettingsOpen(false);
  }, []);

  const handleSettingsChanged = useCallback(async () => {
    await loadSnapshot(false);
  }, [loadSnapshot]);

  // Schedule controls — Run / Pause / Resume. Mirror Fleet.jsx so the
  // workspace page can drive the periodic schedule without the user having
  // to jump back to the Fleet view.
  const [runNowBusy, setRunNowBusy] = useState(false);
  const handleRunNow = useCallback(async () => {
    if (runNowBusy) return;
    setRunNowBusy(true);
    try {
      await runWorkspaceNow(workspaceId);
      setError('');
      await loadSnapshot(false);
    } catch (err) {
      setError(errorMessage(err, 'Failed to start run.'));
    } finally {
      setRunNowBusy(false);
    }
  }, [loadSnapshot, runNowBusy, workspaceId]);

  // Track the run id we asked to cancel so the Stop button stays in a
  // "stopping…" state until the snapshot confirms the run flipped to a
  // terminal state. `assistant_cancel_run` only *signals* the cancel
  // token — the engine flips RunStatus on its next checkpoint — so
  // clearing busy on resolve would re-arm the button while the run is
  // still streaming.
  const [cancellingRunId, setCancellingRunId] = useState<string | null>(null);
  const handleStop = useCallback(
    async (runId: string | null) => {
      if (!runId || cancellingRunId) return;
      setCancellingRunId(runId);
      try {
        await assistantClient.cancelRun(runId);
        setError('');
        await loadSnapshot(false);
      } catch (err) {
        setError(errorMessage(err, 'Failed to stop run.'));
        setCancellingRunId(null);
      }
    },
    [cancellingRunId, loadSnapshot]
  );

  const [pauseBusy, setPauseBusy] = useState(false);
  const handleTogglePause = useCallback(
    async (nextPaused: boolean) => {
      if (pauseBusy) return;
      setPauseBusy(true);
      // Optimistically flip the snapshot's pause flag so the button swaps
      // immediately — the next snapshot poll will reconcile if the backend
      // disagrees.
      setSnapshot((current) => (current ? { ...current, schedulePaused: nextPaused } : current));
      try {
        await setWorkspaceSchedulePaused(workspaceId, nextPaused);
        setError('');
        await loadSnapshot(false);
      } catch (err) {
        setError(errorMessage(err, 'Failed to update pause state.'));
        setSnapshot((current) => (current ? { ...current, schedulePaused: !nextPaused } : current));
      } finally {
        setPauseBusy(false);
      }
    },
    [loadSnapshot, pauseBusy, workspaceId]
  );

  const handleAgentRemove = useCallback(
    async (workspaceAgentId: string) => {
      if (agentBusy) return;
      setAgentBusy(`remove:${workspaceAgentId}`);
      setAgentError('');
      try {
        await workspaceDeleteAgent(workspaceId, workspaceAgentId);
        await loadSnapshot(false);
      } catch (err) {
        setAgentError(errorMessage(err, 'Failed to remove agent.'));
      } finally {
        setAgentBusy('');
      }
    },
    [agentBusy, loadSnapshot, workspaceId]
  );

  useEffect(() => {
    lastLoadedSessionUpdatedAtRef.current = null;
    loadSnapshot(true);
    const interval = window.setInterval(
      () => loadSnapshot(false, LIGHTWEIGHT_SNAPSHOT_OPTIONS),
      REFRESH_INTERVAL_MS
    );
    return () => window.clearInterval(interval);
  }, [loadSnapshot]);

  const memories = snapshot?.memories || [];
  const artifacts = snapshot?.artifacts || [];
  const messages = sessionState?.messages || snapshot?.messages || [];
  const toolCalls = sessionState?.toolCalls || snapshot?.toolCalls || [];
  const streamingText = sessionState?.streamingTextByMessageId || {};
  const isStreaming = sessionState?.isStreaming || false;
  const tasks = snapshot?.tasks || [];
  // The manager session's currently-in-flight run, if any. Drives the
  // header Stop button + hides Run-now while a run is mid-stream.
  // `snapshot.runs` is sorted newest-first by the backend; pick the first
  // non-terminal entry so we cancel the most recent activation.
  const activeRun =
    (snapshot?.runs || []).find((run) => ACTIVE_RUN_STATUSES.includes(run.status)) || null;
  const hasActiveRun = !!activeRun;
  // Clear the "stopping…" lock once the cancelled run leaves the active
  // set. The cancel propagation is async (engine checkpoints), so we
  // can't clear on the cancel call returning.
  useEffect(() => {
    if (!cancellingRunId) return;
    const stillActive = (snapshot?.runs || []).some(
      (run) => run.id === cancellingRunId && ACTIVE_RUN_STATUSES.includes(run.status)
    );
    if (!stillActive) setCancellingRunId(null);
  }, [snapshot, cancellingRunId]);
  const stopBusy = cancellingRunId !== null;

  return (
    <div className={styles.workspacePage}>
      <WorkspaceHeader
        snapshot={snapshot}
        workspaceId={workspaceId}
        isGenericWorkspace={isGenericWorkspace}
        messages={messages}
        memories={memories}
        artifacts={artifacts}
        activePanel={activePanel}
        setActivePanel={setActivePanel}
        onRunNow={handleRunNow}
        onTogglePause={handleTogglePause}
        onStop={handleStop}
        activeRunId={activeRun?.id || null}
        hasActiveRun={hasActiveRun}
        runNowBusy={runNowBusy}
        pauseBusy={pauseBusy}
        stopBusy={stopBusy}
      />

      {error && <div className={styles.errorBanner}>{error}</div>}

      <WorkspaceAttentionBanner tasks={tasks} />

      <div className={styles.workspaceBody}>
        <div
          className={`${styles.workspaceMain} ${isSidePanelOpen ? styles.workspaceMainWithPreview : ''}`}
        >
          <ChatFirstLayout
            sessionId={sessionId}
            workspaceId={workspaceId}
            messages={messages}
            toolCalls={toolCalls}
            streamingText={streamingText}
            isStreaming={isStreaming}
          />
        </div>

        {snapshot && activePanel && previewEntry && (
          <WorkspaceFilePreviewPanel
            workspaceId={workspaceId}
            kind={previewEntry.kind}
            entry={previewEntry.entry}
            onClose={closePreview}
          />
        )}

        {snapshot && activePanel === 'tasks' && viewingTask && (
          <WorkspaceTaskTranscriptPanel task={viewingTask} onClose={closeTaskTranscript} />
        )}

        {snapshot && activePanel && (
          <aside className={styles.workspaceDrawer} aria-label={`${activePanel} drawer`}>
            <div className={styles.workspaceDrawerHeader}>
              <span className={styles.workspaceDrawerTitle}>
                {activePanel.charAt(0).toUpperCase() + activePanel.slice(1)}
              </span>
              <div className={styles.workspaceDrawerActions}>
                {activePanel === 'agents' &&
                  snapshot?.kind !== 'agent' &&
                  workspaceId !== DEFAULT_WORKSPACE_ID && (
                    <button
                      type="button"
                      className={styles.workspaceDrawerAction}
                      onClick={openMemberCreate}
                      disabled={!!agentBusy}
                    >
                      + Add Agent
                    </button>
                  )}
                <button
                  type="button"
                  className={styles.workspaceDrawerClose}
                  onClick={() => setActivePanel(null)}
                  title="Close panel"
                  aria-label="Close panel"
                >
                  ×
                </button>
              </div>
            </div>

            <div className={styles.workspaceDrawerBody}>
              {activePanel === 'agents' && (
                <WorkspaceAgentsPanel
                  workspaceId={workspaceId}
                  snapshot={snapshot}
                  busy={agentBusy}
                  error={agentError}
                  onOpenEdit={openAgentEdit}
                  onRemove={handleAgentRemove}
                />
              )}

              {activePanel === 'tasks' && (
                <WorkspaceTasksPanel
                  workspaceId={workspaceId}
                  tasks={tasks}
                  onChanged={() => loadSnapshot(false)}
                  onViewTask={openTaskTranscript}
                />
              )}

              {activePanel === 'memories' && (
                <WorkspaceFileEntryList
                  entries={memories}
                  emptyMessage="The workspace hasn't stored anything in memory yet."
                  onSelect={(entry) => openPreviewEntry({ kind: 'memory', entry })}
                />
              )}

              {activePanel === 'artifacts' && (
                <ArtifactsList
                  artifacts={artifacts}
                  onSelect={(entry) => openPreviewEntry({ kind: 'artifact', entry })}
                />
              )}
            </div>
          </aside>
        )}
      </div>

      <WorkspaceSettingsModal
        isOpen={settingsOpen}
        onClose={handleSettingsClose}
        workspaceId={workspaceId}
        snapshot={snapshot}
        initialSelection={settingsSelection}
        onChanged={handleSettingsChanged}
      />
    </div>
  );
};

export default Workspace;
