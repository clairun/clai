import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useOutletContext, useParams } from 'react-router-dom';
import type { FleetOutletContext } from '../layouts/FleetLayout';
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
  importWorkspaceFiles,
  listWorkspaceDir,
  markWorkspaceOpened,
  openWorkspacePath,
  runWorkspaceNow,
  searchWorkspaceArtifacts,
  setWorkspaceSchedulePaused,
  setWorkspaceTitle,
} from '../workspace/client';
import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
import type {
  AssistantMessage,
  AssistantRun,
  ToolInvocation,
  WorkspaceDirEntry,
  WorkspaceFileEntry,
  WorkspaceSnapshot,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import styles from './Workspace.module.css';

const DEFAULT_WORKSPACE_ID = 'default';
const REFRESH_INTERVAL_MS = 5000;
const MESSAGE_PAGE_LIMIT = 100;
// Periodic poll skips the session payload (messages/runs/toolCalls are
// kept in sync via the assistant event stream) but still re-walks the
// workspace filesystem so memories created by a running agent surface
// without the user having to re-enter the workspace, and so the artifact
// count stays current. Artifacts themselves are no longer returned here —
// the panel lazy-loads each directory level via workspace_list_dir — so the
// per-tick cost is a memory walk plus a recursive artifact count.
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

// Mirrors the backend `is_usage_limit_error` classifier (local_agent.rs).
// Used purely for presentation: a usage/rate limit is non-retryable until a
// reset time the message already states, so we show it with a calmer style
// (clock) rather than a hard error.
const isUsageLimitError = (message: string): boolean => {
  const m = message.toLowerCase();
  return (
    m.includes('usage limit') ||
    m.includes('session limit') ||
    m.includes('rate limit') ||
    m.includes('rate_limit') ||
    m.includes('quota') ||
    (m.includes("you've hit your") && m.includes('limit'))
  );
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
// Artifacts can number in the tens of thousands, so the panel never loads the
// whole tree. It lazy-loads one directory level at a time via
// `workspace_list_dir`: the root on open, then each folder's children when the
// user expands it. Search bypasses the tree entirely and asks the backend
// (`workspace_search_artifacts`) so it can span folders that were never opened.

// A flattened, depth-tagged row for the virtualized tree view. `entry` is the
// directory listing entry (file or directory); `loading` rows are transient
// placeholders shown while a just-expanded folder's children are in flight.
type ArtifactRow =
  | { kind: 'entry'; key: string; depth: number; entry: WorkspaceDirEntry }
  | { kind: 'loading'; key: string; depth: number };

// Convert a directory-listing file entry into the WorkspaceFileEntry shape the
// preview/onSelect plumbing expects.
const dirEntryToFileEntry = (entry: WorkspaceDirEntry): WorkspaceFileEntry => ({
  path: entry.path,
  relativePath: entry.path,
  name: entry.name,
  viewer: entry.viewer ?? '',
  size: entry.size,
  updatedAt: entry.updatedAt,
  preview: null,
});

// Walk the loaded directory levels into a flat, depth-tagged row list,
// descending only into folders the user has expanded. A folder that's expanded
// but not yet loaded emits a single loading placeholder row.
const flattenLoadedTree = (
  childrenByPath: Map<string, WorkspaceDirEntry[]>,
  expanded: Set<string>
): ArtifactRow[] => {
  const out: ArtifactRow[] = [];
  const walk = (parentPath: string, depth: number) => {
    const children = childrenByPath.get(parentPath);
    if (!children) return;
    for (const entry of children) {
      out.push({ kind: 'entry', key: entry.path, depth, entry });
      if (entry.kind === 'directory' && expanded.has(entry.path)) {
        if (childrenByPath.has(entry.path)) {
          walk(entry.path, depth + 1);
        } else {
          out.push({ kind: 'loading', key: `${entry.path}::loading`, depth: depth + 1 });
        }
      }
    }
  };
  walk('', 0);
  return out;
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
  row: ArtifactRow;
  isExpanded: boolean;
  onToggle: (path: string) => void;
  onSelect?: (entry: WorkspaceFileEntry) => void;
}

const ArtifactTreeRow = ({ row, isExpanded, onToggle, onSelect }: ArtifactTreeRowProps) => {
  if (row.kind === 'loading') {
    return (
      <div
        className={`${styles.fileTreeRow} ${styles.fileTreeRowFile}`}
        style={{ paddingInlineStart: 8 + row.depth * 14 }}
      >
        <span className={styles.fileTreeChevronSlot} />
        <span className={styles.fileTreeName}>Loading…</span>
      </div>
    );
  }

  const { entry, depth } = row;
  const isFolder = entry.kind === 'directory';
  const handleClick = () => {
    if (isFolder) onToggle(entry.path);
    else onSelect?.(dirEntryToFileEntry(entry));
  };
  return (
    <button
      type="button"
      className={`${styles.fileTreeRow} ${isFolder ? styles.fileTreeRowFolder : styles.fileTreeRowFile}`}
      style={{ paddingInlineStart: 8 + depth * 14 }}
      onClick={handleClick}
      title={entry.path}
      aria-expanded={isFolder ? isExpanded : undefined}
    >
      <span className={styles.fileTreeChevronSlot}>
        {isFolder && <ChevronIcon open={isExpanded} />}
      </span>
      <span className={styles.fileTreeIcon}>{isFolder ? <FolderGlyph /> : <FileGlyph />}</span>
      <span className={styles.fileTreeName}>{entry.name}</span>
      <span className={styles.fileTreeMeta}>
        {isFolder
          ? Number(entry.childCount ?? 0)
          : entry.updatedAt
            ? formatTimestamp(entry.updatedAt)
            : ''}
      </span>
    </button>
  );
};

interface ArtifactsListProps {
  workspaceId: string;
  totalCount: number;
  /** Latest mtime (unix ms) across the artifact tree — changes on
   *  content-only edits and renames, which leave `totalCount` unchanged. */
  latestModifiedAt: number;
  onSelect?: (entry: WorkspaceFileEntry) => void;
}

const ARTIFACT_SEARCH_DEBOUNCE_MS = 250;

// Lazy directory-tree browser. Loads the root level on open and each folder's
// children on first expand, caching them by path. The 5s snapshot poll surfaces
// new artifacts by bumping `totalCount`, which we use to silently refresh the
// already-loaded levels so a running agent's output appears without reopening.
const ArtifactsList = ({ workspaceId, totalCount, latestModifiedAt, onSelect }: ArtifactsListProps) => {
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [childrenByPath, setChildrenByPath] = useState<Map<string, WorkspaceDirEntry[]>>(
    () => new Map()
  );
  const loadingRef = useRef<Set<string>>(new Set());
  const autoExpandedRef = useRef(false);

  const [searchResults, setSearchResults] = useState<WorkspaceFileEntry[] | null>(null);
  const [searching, setSearching] = useState(false);
  const trimmedQuery = query.trim();

  // Mirror of `childrenByPath` so callbacks/effects can read the latest
  // value without subscribing to it (avoids re-firing the workspace-change
  // effect and the artifact-tree refresh effect on every map update).
  const childrenByPathRef = useRef(childrenByPath);
  useEffect(() => {
    childrenByPathRef.current = childrenByPath;
  });

  // Load (or reload) a single directory level. `force` bypasses the cache so the
  // poll-driven refresh can pick up newly created files. The cache check
  // reads the latest `childrenByPath` via the ref so this callback no longer
  // needs the map in its dep array.
  const loadDir = useCallback(
    async (path: string, force = false) => {
      if (loadingRef.current.has(path)) return;
      if (!force && childrenByPathRef.current.has(path)) return;
      loadingRef.current.add(path);
      try {
        const entries = await listWorkspaceDir(workspaceId, path);
        setChildrenByPath((prev) => {
          const next = new Map(prev);
          next.set(path, entries);
          return next;
        });
      } catch (error) {
        console.error(`Failed to list workspace dir "${path}":`, error);
      } finally {
        loadingRef.current.delete(path);
      }
    },
    [workspaceId]
  );

  // Reset and load the root when the workspace changes. `loadDir` is keyed
  // on `workspaceId` and a ref-mirrored `childrenByPath`, so its identity
  // is stable across load-driven map updates and adding it to deps doesn't
  // loop.
  useEffect(() => {
    autoExpandedRef.current = false;
    loadingRef.current = new Set();
    setExpanded(new Set());
    setChildrenByPath(new Map());
    void loadDir('', true);
  }, [workspaceId, loadDir]);

  // When the artifact tree changes, refresh every directory level we've
  // already loaded so the open tree stays live. Keyed on the count (files
  // created/removed) AND the tree's latest mtime — a content-only edit or a
  // rename leaves the count unchanged but must still refresh the listed
  // timestamps. Bounded by how many folders the user has expanded, not by
  // total artifact count. `childrenByPath` is read via the ref so this
  // effect doesn't re-fire on every load-driven map update; `loadDir` is
  // stable now that its only real dep is `workspaceId`.
  useEffect(() => {
    if (childrenByPathRef.current.size === 0) return;
    for (const path of childrenByPathRef.current.keys()) {
      void loadDir(path, true);
    }
  }, [totalCount, latestModifiedAt, loadDir]);

  // Auto-expand a sole top-level folder once, so repo-rooted artifacts like
  // `work/<repo>/...` reveal their first level without an extra click.
  useEffect(() => {
    if (autoExpandedRef.current) return;
    const root = childrenByPath.get('');
    if (!root) return;
    autoExpandedRef.current = true;
    if (root.length === 1 && root[0]!.kind === 'directory') {
      const only = root[0]!.path;
      setExpanded(new Set([only]));
      void loadDir(only);
    }
  }, [childrenByPath, loadDir]);

  // Debounced server-side search. Empty query → tree view.
  useEffect(() => {
    if (!trimmedQuery) {
      setSearchResults(null);
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const handle = window.setTimeout(() => {
      searchWorkspaceArtifacts(workspaceId, trimmedQuery)
        .then((results) => {
          if (!cancelled) setSearchResults(results);
        })
        .catch((error) => {
          console.error('Failed to search workspace artifacts:', error);
          if (!cancelled) setSearchResults([]);
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, ARTIFACT_SEARCH_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [workspaceId, trimmedQuery]);

  const handleToggle = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        const next = new Set(prev);
        if (next.has(path)) {
          next.delete(path);
        } else {
          next.add(path);
          void loadDir(path);
        }
        return next;
      });
    },
    [loadDir]
  );

  const visibleRows = useMemo(
    () => flattenLoadedTree(childrenByPath, expanded),
    [childrenByPath, expanded]
  );

  const itemKey = useCallback((row: ArtifactRow) => row.key, []);
  const renderItem = useCallback(
    (row: ArtifactRow) => (
      <ArtifactTreeRow
        row={row}
        isExpanded={row.kind === 'entry' && expanded.has(row.entry.path)}
        onToggle={handleToggle}
        onSelect={onSelect}
      />
    ),
    [expanded, handleToggle, onSelect]
  );

  return (
    <div className={styles.searchableList}>
      {totalCount > 0 && (
        <input
          type="text"
          className={styles.searchInput}
          value={query}
          onChange={(event: React.ChangeEvent<HTMLInputElement>) => setQuery(event.target.value)}
          placeholder={`Search artifacts (${totalCount})`}
          aria-label="Search artifacts"
        />
      )}
      {trimmedQuery ? (
        searching && searchResults === null ? (
          <div className={styles.drawerEmpty}>Searching…</div>
        ) : (
          <WorkspaceFileEntryList
            entries={searchResults || []}
            emptyMessage={`No artifacts match "${query}".`}
            onSelect={onSelect}
          />
        )
      ) : totalCount === 0 ? (
        <div className={styles.drawerEmpty}>No artifacts in this workspace yet.</div>
      ) : visibleRows.length === 0 ? (
        <div className={styles.drawerEmpty}>Loading…</div>
      ) : (
        <WorkspaceVirtualizedList
          items={visibleRows}
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
  messageCount,
  memories,
  artifactCount,
  activePanel,
  setActivePanel,
  onRunNow,
  onTogglePause,
  onStop,
  onTitleSaved,
  activeRunId,
  hasActiveRun,
  runNowBusy,
  pauseBusy,
  stopBusy,
}: {
  snapshot: WorkspaceSnapshot | null;
  workspaceId: string;
  isGenericWorkspace: boolean;
  // Total messages in the conversation (including not-yet-loaded history
  // and rotation ancestors), not just the loaded window.
  messageCount: number;
  memories: WorkspaceFileEntry[];
  artifactCount: number;
  activePanel: ActivePanel;
  setActivePanel: React.Dispatch<React.SetStateAction<ActivePanel>>;
  onTitleSaved: (title: string) => void;
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

  // ── Inline title rename ────────────────────────────────────────────
  // Click the title to edit it in place; Enter/blur commits, Escape
  // cancels. The generic workspace has no real title to rename.
  const currentTitle = snapshot?.title || (isGenericWorkspace ? 'Workspace' : workspaceId);
  const canEditTitle = !isGenericWorkspace && !!snapshot;
  const [isEditingTitle, setIsEditingTitle] = useState(false);
  const [draftTitle, setDraftTitle] = useState('');
  const [savingTitle, setSavingTitle] = useState(false);
  const titleInputRef = useRef<HTMLInputElement>(null);
  // Escape unmounts the input, whose trailing blur would otherwise commit the
  // edited value. This flag makes the blur a no-op for that one transition.
  const skipBlurCommitRef = useRef(false);

  useEffect(() => {
    if (isEditingTitle) {
      titleInputRef.current?.focus();
      titleInputRef.current?.select();
    }
  }, [isEditingTitle]);

  const beginEditTitle = () => {
    if (!canEditTitle) return;
    skipBlurCommitRef.current = false;
    setDraftTitle(snapshot?.title || '');
    setIsEditingTitle(true);
  };

  const cancelEditTitle = () => {
    skipBlurCommitRef.current = true;
    setIsEditingTitle(false);
    setDraftTitle('');
  };

  const commitTitle = async () => {
    if (skipBlurCommitRef.current) {
      skipBlurCommitRef.current = false;
      return;
    }
    if (savingTitle) return;
    const trimmed = draftTitle.trim();
    // Empty, too long, or unchanged → close without a write.
    if (!trimmed || trimmed.length > 100 || trimmed === (snapshot?.title || '').trim()) {
      cancelEditTitle();
      return;
    }
    setSavingTitle(true);
    try {
      await setWorkspaceTitle(workspaceId, trimmed);
      onTitleSaved(trimmed);
      setIsEditingTitle(false);
      setDraftTitle('');
    } catch {
      // Leave the field open so the user can retry or press Escape.
    } finally {
      setSavingTitle(false);
    }
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
        {isEditingTitle ? (
          <input
            ref={titleInputRef}
            type="text"
            className={styles.titleInput}
            value={draftTitle}
            maxLength={100}
            disabled={savingTitle}
            onChange={(e) => setDraftTitle(e.target.value)}
            onBlur={() => void commitTitle()}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                void commitTitle();
              } else if (e.key === 'Escape') {
                e.preventDefault();
                cancelEditTitle();
              }
            }}
            aria-label="Workspace title"
          />
        ) : (
          <h1
            className={`${styles.title} ${canEditTitle ? styles.titleEditable : ''}`}
            onClick={beginEditTitle}
            title={canEditTitle ? 'Click to rename' : undefined}
            role={canEditTitle ? 'button' : undefined}
            tabIndex={canEditTitle ? 0 : undefined}
            onKeyDown={
              canEditTitle
                ? (e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      beginEditTitle();
                    }
                  }
                : undefined
            }
          >
            {currentTitle}
          </h1>
        )}
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
        {renderCounter(null, messageCount, 'msgs', false)}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('agents', assignedAgentCount, 'agents')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('tasks', taskCount, 'tasks', true, activeTaskCount)}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('memories', memories.length, 'memories')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('artifacts', artifactCount, 'artifacts')}
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
  runError: string | null;
  runErrorIsLimit: boolean;
  runStartedAt: number | null;
  queuedMessageIds: string[];
  onDeleteQueuedMessage: (messageId: string) => void;
  hasOlderMessages: boolean;
  isLoadingOlderMessages: boolean;
  onLoadOlderMessages: () => void;
}

const ChatFirstLayout = ({
  sessionId,
  workspaceId,
  messages,
  toolCalls,
  streamingText,
  isStreaming,
  runError,
  runErrorIsLimit,
  runStartedAt,
  queuedMessageIds,
  onDeleteQueuedMessage,
  hasOlderMessages,
  isLoadingOlderMessages,
  onLoadOlderMessages,
}: ChatFirstLayoutProps) => {
  const cardRef = useRef<HTMLDivElement | null>(null);

  // Publish the conversation card's viewport geometry as document-level
  // CSS vars so the fixed input bar (TerminalEmulator) can align itself
  // with the conversation column instead of spanning the whole detail
  // pane (same pattern as --terminal-height / --fleet-rail-width). The
  // card's own ResizeObserver misses pure position shifts (margin:auto
  // recentering while width stays at max), so the parent — whose content
  // box changes whenever the side-panel padding animates or the window
  // resizes — is observed too. Cleared on unmount so other routes fall
  // back to the pane-centered default.
  useLayoutEffect(() => {
    const node = cardRef.current;
    if (!node || typeof ResizeObserver === 'undefined') return undefined;

    let raf: number | null = null;
    const publish = () => {
      raf = null;
      const rect = node.getBoundingClientRect();
      // When the pane is too narrow the card slides left out of it (its
      // left edge clips under the workspace rail — see .chatFirstContent),
      // but the input bar must stay inside the pane. Publish the VISIBLE
      // strip instead of the raw card rect, floored at the same minimum
      // as the CSS --chat-min-width so the bar never shrinks below it.
      const paneLeft = node.parentElement?.getBoundingClientRect().left ?? rect.left;
      const visibleLeft = Math.max(rect.left, paneLeft);
      const width = Math.max(rect.right - visibleLeft, 420);
      const rootStyle = document.documentElement.style;
      rootStyle.setProperty('--chat-card-center', `${visibleLeft + width / 2}px`);
      rootStyle.setProperty('--chat-card-width', `${width}px`);
    };
    const schedule = () => {
      if (raf == null) raf = window.requestAnimationFrame(publish);
    };

    publish();
    const observer = new ResizeObserver(schedule);
    observer.observe(node);
    if (node.parentElement) observer.observe(node.parentElement);

    return () => {
      observer.disconnect();
      if (raf != null) window.cancelAnimationFrame(raf);
      const rootStyle = document.documentElement.style;
      rootStyle.removeProperty('--chat-card-center');
      rootStyle.removeProperty('--chat-card-width');
    };
  }, []);

  return (
    <div className={styles.chatFirstContent} ref={cardRef}>
    {messages.length > 0 ? (
      <>
        {/* Keyed by workspace: this component instance is reused across
            workspace→workspace navigations, so without the key the list
            carries over the previous workspace's scroll position (and its
            measured-height cache) instead of opening at the bottom. */}
        <ChatMessageList
          key={workspaceId}
          messages={messages}
          toolCalls={toolCalls}
          streamingText={streamingText}
          isStreaming={isStreaming}
          runError={runError}
          runErrorIsLimit={runErrorIsLimit}
          runStartedAt={runStartedAt}
          queuedMessageIds={queuedMessageIds}
          onDeleteQueuedMessage={onDeleteQueuedMessage}
          hasOlderMessages={hasOlderMessages}
          isLoadingOlderMessages={isLoadingOlderMessages}
          onLoadOlderMessages={onLoadOlderMessages}
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
};

const Workspace = () => {
  const params = useParams();
  const { toggleChat } = useChatManager() as { toggleChat: () => void };
  // Provided by FleetLayout's <Outlet>; lets us refresh the workspace rail
  // immediately after changes (e.g. a title rename) instead of waiting for
  // its 5s poll. Optional-chained so the page is resilient if ever rendered
  // outside that layout.
  const { loadWorkspaces } = useOutletContext<FleetOutletContext>() ?? {};
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

      const effectiveOptions = options ?? LIGHTWEIGHT_SNAPSHOT_OPTIONS;
      const isLightweight = effectiveOptions.includeSessionPayload === false;

      try {
        const nextSnapshot = await getWorkspaceSnapshot(workspaceId, effectiveOptions);
        setSnapshot((current) => {
          if (!isLightweight || !current) {
            return nextSnapshot;
          }

          // Lightweight refresh: the backend skipped the session payload
          // (messages/runs/toolCalls live in the assistant event store),
          // so preserve those from the prior snapshot. Memories and the
          // artifact count ARE re-fetched so a running agent's writes appear
          // without the user having to re-enter the workspace (the artifacts
          // panel itself lazy-loads directory levels on its own).
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
            needsInitialHydration || (hasUnloadedUpdate && !existingSession?.isStreaming);

          if (shouldHydrateSession) {
            const [messagePage, runs] = await Promise.all([
              assistantClient.loadSessionMessagesPage({
                sessionId: nextSnapshot.session.id,
                limit: MESSAGE_PAGE_LIMIT,
                includeAncestors: true,
              }),
              assistantClient.listRuns(nextSnapshot.session.id),
            ]);

            store.loadSessionData(
              nextSnapshot.session.id,
              nextSnapshot.session,
              messagePage.messages,
              runs,
              messagePage.toolCalls,
              nextSnapshot.queuedMessageIds || [],
              messagePage.nextCursor ?? null,
              messagePage.hasMore,
              messagePage.totalCount
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
  // Reflect an inline title rename immediately on the page, then refresh the
  // Fleet rail so its row updates at once rather than on the next 5s poll.
  const handleTitleSaved = useCallback(
    (title: string) => {
      setSnapshot((current) => (current ? { ...current, title } : current));
      void loadWorkspaces?.();
    },
    [loadWorkspaces]
  );

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
  // `artifacts` is now intentionally empty in the snapshot — the panel lazy-
  // loads each directory level on demand. It's kept only so navigatePreviewFile
  // can still resolve a clicked sibling against any entries it has seen.
  const artifacts = snapshot?.artifacts || [];
  const artifactCount = Number(snapshot?.artifactCount ?? 0);

  // Open another workspace file in the preview — used when a link inside a
  // previewed file points at a sibling (an index page linking to a report, a
  // doc linking to another .md). Resolve against the tracked artifact/memory
  // lists to keep the right kind/label; otherwise synthesize a minimal entry
  // so any in-root file still opens (the panel derives its viewer from path).
  const navigatePreviewFile = useCallback(
    (path: string) => {
      const artifactMatch = artifacts.find((item) => item.path === path);
      const memoryMatch = artifactMatch ? null : memories.find((item) => item.path === path);
      const kind: PreviewEntry['kind'] = memoryMatch ? 'memory' : 'artifact';
      const entry: WorkspaceFileEntry = artifactMatch ??
        memoryMatch ?? {
          path,
          relativePath: path,
          name: path.slice(path.lastIndexOf('/') + 1),
          viewer: '',
          size: null,
          updatedAt: null,
          preview: null,
        };
      patchWorkspaceUi({ previewEntry: { kind, entry }, viewingTask: null });
    },
    [artifacts, memories, patchWorkspaceUi]
  );
  const messages = sessionState?.messages || snapshot?.messages || [];
  // Conversation total from the backend page responses (kept live by the
  // store as messages stream in); before the first page load reports it,
  // the loaded window is the best available answer.
  const totalMessageCount = sessionState?.totalMessageCount ?? messages.length;
  const toolCalls = sessionState?.toolCalls || snapshot?.toolCalls || [];
  const streamingText = sessionState?.streamingTextByMessageId || {};
  const isStreaming = sessionState?.isStreaming || false;
  const runStartedAt = sessionState?.runStartedAt ?? null;
  // Store is the live source once the session is hydrated; the snapshot
  // covers the first render before hydration.
  const queuedMessageIds = sessionState?.queuedMessageIds ?? snapshot?.queuedMessageIds ?? [];
  const hasOlderMessages = !!sessionState?.hasOlderMessages;
  const isLoadingOlderMessages = !!sessionState?.isLoadingOlderMessages;
  const handleDeleteQueuedMessage = useCallback(
    (messageId: string) => {
      if (!sessionId) return;
      // MessageDeleted from the backend removes it from the store; on a
      // lost race ("already picked up") the chip clears via
      // queued_messages_delivered instead, so both outcomes self-resolve.
      assistantClient.deleteQueuedMessage(sessionId, messageId).catch((err) => {
        console.error('[Workspace] Failed to delete queued message:', err);
      });
    },
    [sessionId]
  );
  const handleLoadOlderMessages = useCallback(() => {
    if (!sessionId) return;
    const store = useAssistantStore.getState();
    const current = store.sessions[sessionId];
    if (!current?.hasOlderMessages || !current.olderMessageCursor || current.isLoadingOlderMessages) {
      return;
    }

    store.setOlderMessagesLoading(sessionId, true);
    assistantClient
      .loadSessionMessagesPage({
        sessionId,
        before: current.olderMessageCursor,
        limit: MESSAGE_PAGE_LIMIT,
        includeAncestors: true,
      })
      .then((page) => {
        useAssistantStore
          .getState()
          .prependMessagePage(
            sessionId,
            page.messages,
            page.toolCalls,
            page.nextCursor ?? null,
            page.hasMore,
            page.totalCount
          );
      })
      .catch((err) => {
        console.error('[Workspace] Failed to load older messages:', err);
        useAssistantStore.getState().setOlderMessagesLoading(sessionId, false);
      });
  }, [sessionId]);
  const tasks = snapshot?.tasks || [];
  // The manager session's currently-in-flight run, if any. Drives the
  // header Stop button + hides Run-now while a run is mid-stream.
  // `snapshot.runs` is sorted newest-first by the backend; pick the first
  // non-terminal entry so we cancel the most recent activation.
  const activeRun =
    (snapshot?.runs || []).find((run) => ACTIVE_RUN_STATUSES.includes(run.status)) || null;
  const hasActiveRun = !!activeRun;
  // Surface the most recent run's failure in the chat. Derived from the
  // newest run, so it clears automatically when the next run starts. Without
  // this, a failed turn (e.g. a provider usage/token limit) shows nothing.
  const lastRun = getLastRunInfo(sessionState?.runs || snapshot?.runs);
  const runError =
    lastRun?.status === 'failed' ? lastRun.error?.trim() || 'The run failed.' : null;
  const runErrorIsLimit = runError ? isUsageLimitError(runError) : false;
  // Tell the backend this workspace is being viewed so the rail clears its
  // "unread" indicator. Keyed on isStreaming too: a run that finishes while
  // the user is watching re-stamps last_opened_at past the completion, and
  // the cleanup stamps on leave so everything that was on screen at that
  // point counts as seen (a run still in flight when they navigate away
  // will complete *after* this stamp → unread). Best-effort fire-and-forget.
  useEffect(() => {
    markWorkspaceOpened(workspaceId).catch(() => {});
    return () => {
      markWorkspaceOpened(workspaceId).catch(() => {});
    };
  }, [workspaceId, isStreaming]);
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

  // Drawer header actions for the artifacts panel: hand the workspace
  // folder to the user's editor/terminal (Settings → Applications), and
  // import files picked in the native dialog (under Flatpak the
  // FileChooser portal grants access to the picked files).
  const handleOpenWorkspaceIn = useCallback(
    async (target: 'editor' | 'terminal') => {
      try {
        await openWorkspacePath(workspaceId, null, target);
      } catch (err) {
        setError(errorMessage(err, `Failed to open the workspace in the ${target}.`));
      }
    },
    [workspaceId]
  );

  const handleAddFiles = useCallback(async () => {
    try {
      const picked = await openFileDialog({ multiple: true, title: 'Add files to workspace' });
      const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter(
        (p): p is string => typeof p === 'string'
      );
      if (paths.length === 0) return;
      await importWorkspaceFiles(workspaceId, paths);
      await loadSnapshot(false);
    } catch (err) {
      setError(errorMessage(err, 'Failed to add files.'));
    }
  }, [workspaceId, loadSnapshot]);

  return (
    <div className={styles.workspacePage}>
      <WorkspaceHeader
        snapshot={snapshot}
        workspaceId={workspaceId}
        isGenericWorkspace={isGenericWorkspace}
        messageCount={totalMessageCount}
        memories={memories}
        artifactCount={artifactCount}
        activePanel={activePanel}
        setActivePanel={setActivePanel}
        onTitleSaved={handleTitleSaved}
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
          className={`${styles.workspaceMain} ${
            isSidePanelOpen
              ? styles.workspaceMainWithPreview
              : activePanel
                ? styles.workspaceMainWithDrawer
                : ''
          }`}
        >
          <ChatFirstLayout
            sessionId={sessionId}
            workspaceId={workspaceId}
            messages={messages}
            toolCalls={toolCalls}
            streamingText={streamingText}
            isStreaming={isStreaming}
            runError={runError}
            runErrorIsLimit={runErrorIsLimit}
            runStartedAt={runStartedAt}
            queuedMessageIds={queuedMessageIds}
            onDeleteQueuedMessage={handleDeleteQueuedMessage}
            hasOlderMessages={hasOlderMessages}
            isLoadingOlderMessages={isLoadingOlderMessages}
            onLoadOlderMessages={handleLoadOlderMessages}
          />
        </div>

        {snapshot && activePanel && previewEntry && (
          <WorkspaceFilePreviewPanel
            workspaceId={workspaceId}
            kind={previewEntry.kind}
            entry={previewEntry.entry}
            onClose={closePreview}
            onNavigate={navigatePreviewFile}
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
                {activePanel === 'artifacts' && (
                  <>
                    <button
                      type="button"
                      className={styles.workspaceDrawerIconAction}
                      onClick={handleAddFiles}
                      title="Add files to the workspace"
                      aria-label="Add files to the workspace"
                    >
                      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                        <polyline points="14 2 14 8 20 8" />
                        <line x1="12" y1="12" x2="12" y2="18" />
                        <line x1="9" y1="15" x2="15" y2="15" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className={styles.workspaceDrawerIconAction}
                      onClick={() => handleOpenWorkspaceIn('editor')}
                      title="Open the workspace in your editor"
                      aria-label="Open the workspace in your editor"
                    >
                      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <polyline points="16 18 22 12 16 6" />
                        <polyline points="8 6 2 12 8 18" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className={styles.workspaceDrawerIconAction}
                      onClick={() => handleOpenWorkspaceIn('terminal')}
                      title="Open a terminal at the workspace folder"
                      aria-label="Open a terminal at the workspace folder"
                    >
                      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <polyline points="4 17 10 11 4 5" />
                        <line x1="12" y1="19" x2="20" y2="19" />
                      </svg>
                    </button>
                  </>
                )}
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
                  workspaceId={workspaceId}
                  totalCount={artifactCount}
                  latestModifiedAt={Number(snapshot?.artifactLatestModifiedAt ?? 0)}
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
