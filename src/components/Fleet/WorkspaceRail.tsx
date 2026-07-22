import React, { useMemo, useState } from 'react';
import type { WorkspaceListEntry } from '../../generated/bindings';
import {
  CARD_STATUS_LABEL,
  deriveCardStatus,
  formatScheduleLabel,
  num,
} from '../../fleet/workspaceStatus';
import styles from './WorkspaceRail.module.css';

interface WorkspaceRailProps {
  workspaces: WorkspaceListEntry[];
  selectedId: string | null;
  /** Per-workspace pending approval/path-grant count (merged). */
  attentionCounts: Record<string, number>;
  /** Per-workspace in-flight interactive run count. */
  activeRuns: Record<string, number>;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onSelect: (id: string) => void;
  onCreate: () => void;
  onRunNow: (id: string) => void;
  onTogglePause: (id: string, currentlyPaused: boolean) => void;
  onToggleStar: (id: string, currentlyStarred: boolean) => void;
  onSettings: (id: string) => void;
  onFork: (id: string) => void;
  onDelete: (id: string, title: string) => void;
  runNowBusyId: string | null;
  forkBusyId: string | null;
  pauseBusyId: string | null;
  /** Global scheduler pause (overlay across every workspace). */
  schedulerPaused: boolean;
  schedulerPauseBusy: boolean;
  onToggleSchedulerPaused: () => void;
  /** Drop handler for an artifact dragged from the artifacts drawer onto a
   *  workspace row — copies it into that workspace. */
  onArtifactDrop?: (
    destWorkspaceId: string,
    drag: { workspaceId: string; path: string; kind: string; name: string }
  ) => void;
}

const isProcessing = (
  ws: WorkspaceListEntry,
  activeRuns: Record<string, number>,
): boolean => num(ws.runningTaskCount) > 0 || (activeRuns[ws.id] || 0) > 0;

const hasAttention = (
  ws: WorkspaceListEntry,
  attentionCounts: Record<string, number>,
): boolean =>
  (attentionCounts[ws.id] || 0) > 0 ||
  num(ws.failedTaskCount) > 0 ||
  num(ws.blockedTaskCount) > 0;

type RailSection = {
  key: 'attention' | 'starred' | 'recent';
  label: string;
  items: WorkspaceListEntry[];
};

const RunIcon = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
    <path d="M8 5v14l11-7z" />
  </svg>
);

const PauseIcon = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
    <rect x="6" y="5" width="4" height="14" rx="1" />
    <rect x="14" y="5" width="4" height="14" rx="1" />
  </svg>
);

const StarIcon = ({ filled }: { filled: boolean }) => (
  <svg
    width="12"
    height="12"
    viewBox="0 0 24 24"
    fill={filled ? 'currentColor' : 'none'}
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
  </svg>
);

/**
 * Persistent left navigator for the unified Fleet/Workspace view. Lists
 * every workspace grouped into explicit, labeled sections (Claude-style)
 * so ordering is never a mystery:
 *
 *   1. "Needs attention" — pending approvals, failed or blocked tasks.
 *   2. "Starred" — user-pinned workspaces (star via hover icon or ⋯ menu).
 *   3. "Recent" — everything else, most-recently-updated first.
 *
 * Within each section rows keep the recency order. Scheduled workspaces
 * are NOT sorted separately — their cadence renders as a per-row label,
 * an attribute rather than a position. Section headers only appear when
 * grouping is actually in effect (a bare recency list stays headerless).
 *
 * Collapsible: the collapsed state shows just a status dot + initial,
 * with the full title on hover (title attr) and thin dividers between
 * sections. The host owns the collapsed flag (persisted to localStorage)
 * and all data/actions; this component is presentational plus a small
 * amount of per-row menu state.
 */
const WorkspaceRail = ({
  workspaces,
  selectedId,
  attentionCounts,
  activeRuns,
  collapsed,
  onToggleCollapsed,
  onSelect,
  onCreate,
  schedulerPaused,
  schedulerPauseBusy,
  onToggleSchedulerPaused,
  onRunNow,
  onTogglePause,
  onToggleStar,
  onSettings,
  onFork,
  onDelete,
  runNowBusyId,
  forkBusyId,
  pauseBusyId,
  onArtifactDrop,
}: WorkspaceRailProps) => {
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);

  // Pure recency sort — grouping happens per-section below.
  const sorted = useMemo(
    () => [...workspaces].sort((a, b) => num(b.updatedAt) - num(a.updatedAt)),
    [workspaces],
  );

  // Name filter. Applied only when expanded — collapsed has no input, so
  // it shows the full list. Case-insensitive substring match on title.
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (collapsed || !q) return sorted;
    return sorted.filter((ws) => (ws.title || '').toLowerCase().includes(q));
  }, [sorted, query, collapsed]);

  // Partition into labeled sections. Attention outranks starred: a starred
  // workspace that needs input surfaces under "Needs attention" (the star
  // state stays visible via the hover toggle / menu). Empty sections are
  // dropped entirely.
  const sections = useMemo<RailSection[]>(() => {
    const attention: WorkspaceListEntry[] = [];
    const starred: WorkspaceListEntry[] = [];
    const recent: WorkspaceListEntry[] = [];
    for (const ws of visible) {
      if (hasAttention(ws, attentionCounts)) attention.push(ws);
      else if (ws.starred) starred.push(ws);
      else recent.push(ws);
    }
    return [
      { key: 'attention' as const, label: 'Needs attention', items: attention },
      { key: 'starred' as const, label: 'Starred', items: starred },
      { key: 'recent' as const, label: 'Recent', items: recent },
    ].filter((section) => section.items.length > 0);
  }, [visible, attentionCounts]);

  // A lone "Recent" section is just a plain list — no header noise.
  const showHeaders =
    sections.length > 1 || (sections.length === 1 && sections[0]?.key !== 'recent');

  const renderRow = (ws: WorkspaceListEntry) => {
    const processing = isProcessing(ws, activeRuns);
    const pending = attentionCounts[ws.id] || 0;
    const status = deriveCardStatus(ws, processing, pending > 0);
    const isSelected = ws.id === selectedId;
    const scheduleLabel = formatScheduleLabel(ws.scheduleKind);
    const isPaused = !!ws.schedulePaused;
    const isStarred = !!ws.starred;
    const attentionCount = pending + num(ws.failedTaskCount) + num(ws.blockedTaskCount);
    // A run completed since the user last opened this workspace.
    // Suppressed while selected — the open page is marking it seen.
    const isUnread = !!ws.unread && !isSelected;
    const initial = (ws.title || '?').trim().charAt(0).toUpperCase() || '?';
    const rowClasses = [styles.row, isSelected ? styles.rowSelected : ''].join(' ');

    return (
      <div
        key={ws.id}
        className={`${rowClasses}${
          dropTargetId === ws.id ? ` ${styles.rowDropTarget}` : ''
        }`}
        onClick={() => onSelect(ws.id)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onSelect(ws.id);
        }}
        onDragOver={(e) => {
          if (!onArtifactDrop) return;
          if (!e.dataTransfer.types.includes('application/x-clai-artifact')) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = 'copy';
          if (dropTargetId !== ws.id) setDropTargetId(ws.id);
        }}
        onDragLeave={() => {
          setDropTargetId((prev) => (prev === ws.id ? null : prev));
        }}
        onDrop={(e) => {
          if (!onArtifactDrop) return;
          const raw = e.dataTransfer.getData('application/x-clai-artifact');
          setDropTargetId(null);
          if (!raw) return;
          e.preventDefault();
          try {
            const drag = JSON.parse(raw);
            if (
              drag &&
              typeof drag.path === 'string' &&
              typeof drag.workspaceId === 'string' &&
              typeof drag.kind === 'string' &&
              typeof drag.name === 'string'
            ) {
              onArtifactDrop(ws.id, drag);
            }
          } catch {
            // Ignore malformed / foreign drops.
          }
        }}
        title={collapsed ? ws.title : undefined}
      >
        <span
          className={`${styles.statusDot} ${styles[`statusDot_${status}`]}`}
          aria-hidden="true"
          title={CARD_STATUS_LABEL[status]}
        />
        {collapsed ? (
          <>
            <span className={styles.collapsedInitial}>{initial}</span>
            {attentionCount > 0 ? (
              <span className={styles.collapsedBadge} />
            ) : isUnread ? (
              <span className={styles.collapsedUnreadDot} title="New activity" />
            ) : null}
          </>
        ) : (
          <>
            <span className={styles.rowBody}>
              <span className={styles.rowTitle}>{ws.title}</span>
              {ws.scheduleEnabled && (
                <span
                  className={`${styles.rowMeta} ${isPaused ? styles.rowMetaPaused : ''}`}
                >
                  {isPaused
                    ? `Paused${scheduleLabel ? ` · ${scheduleLabel}` : ''}`
                    : scheduleLabel || 'Scheduled'}
                </span>
              )}
            </span>

            {attentionCount > 0 && (
              <span className={styles.attentionBadge} title="Needs attention">
                {attentionCount}
              </span>
            )}

            {isUnread && attentionCount === 0 && (
              <span
                className={styles.unreadDot}
                title="New activity since you last opened this workspace"
                aria-label="Unread activity"
              />
            )}

            <span className={styles.rowActions}>
              <button
                type="button"
                className={`${styles.iconButton} ${isStarred ? styles.starButtonActive : ''}`}
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleStar(ws.id, isStarred);
                }}
                title={isStarred ? 'Unstar workspace' : 'Star workspace'}
                aria-label={isStarred ? 'Unstar workspace' : 'Star workspace'}
                aria-pressed={isStarred}
              >
                <StarIcon filled={isStarred} />
              </button>
              {ws.scheduleEnabled && (
                <>
                  <button
                    type="button"
                    className={styles.iconButton}
                    onClick={(e) => {
                      e.stopPropagation();
                      onRunNow(ws.id);
                    }}
                    disabled={processing || runNowBusyId === ws.id}
                    title={processing ? 'Already running' : 'Run now'}
                    aria-label="Run now"
                  >
                    <RunIcon />
                  </button>
                  <button
                    type="button"
                    className={styles.iconButton}
                    onClick={(e) => {
                      e.stopPropagation();
                      onTogglePause(ws.id, isPaused);
                    }}
                    disabled={pauseBusyId === ws.id}
                    title={isPaused ? 'Resume schedule' : 'Pause schedule'}
                    aria-label={isPaused ? 'Resume schedule' : 'Pause schedule'}
                  >
                    <PauseIcon />
                  </button>
                </>
              )}
              <button
                type="button"
                className={styles.iconButton}
                onClick={(e) => {
                  e.stopPropagation();
                  setOpenMenuId((cur) => (cur === ws.id ? null : ws.id));
                }}
                title="More actions"
                aria-label="More actions"
                aria-haspopup="menu"
                aria-expanded={openMenuId === ws.id}
              >
                ⋯
              </button>
            </span>

            {openMenuId === ws.id && (
              <>
                <button
                  type="button"
                  className={styles.menuBackdrop}
                  aria-hidden="true"
                  tabIndex={-1}
                  onClick={(e) => {
                    e.stopPropagation();
                    setOpenMenuId(null);
                  }}
                />
                <div className={styles.menu} role="menu">
                  <button
                    type="button"
                    className={styles.menuItem}
                    role="menuitem"
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuId(null);
                      onToggleStar(ws.id, isStarred);
                    }}
                  >
                    {isStarred ? 'Unstar workspace' : 'Star workspace'}
                  </button>
                  <button
                    type="button"
                    className={styles.menuItem}
                    role="menuitem"
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuId(null);
                      onSettings(ws.id);
                    }}
                  >
                    Settings
                  </button>
                  <button
                    type="button"
                    className={styles.menuItem}
                    role="menuitem"
                    disabled={forkBusyId === ws.id}
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuId(null);
                      onFork(ws.id);
                    }}
                  >
                    {forkBusyId === ws.id ? 'Forking…' : 'Fork workspace'}
                  </button>
                  <button
                    type="button"
                    className={`${styles.menuItem} ${styles.menuItemDanger}`}
                    role="menuitem"
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuId(null);
                      onDelete(ws.id, ws.title);
                    }}
                  >
                    Delete
                  </button>
                </div>
              </>
            )}
          </>
        )}
      </div>
    );
  };

  return (
    <nav
      className={`${styles.rail} ${collapsed ? styles.railCollapsed : ''}`}
      aria-label="Workspaces"
    >
      <div className={styles.railHeader}>
        <button
          type="button"
          className={styles.collapseToggle}
          onClick={onToggleCollapsed}
          title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <line x1="3" y1="6" x2="21" y2="6" />
            <line x1="3" y1="12" x2="21" y2="12" />
            <line x1="3" y1="18" x2="21" y2="18" />
          </svg>
        </button>
        {!collapsed && <span className={styles.railTitle}>Workspaces</span>}
        {!collapsed && <span className={styles.railCount}>{workspaces.length}</span>}
        {!collapsed && schedulerPaused && (
          <span
            className={styles.pausedPill}
            title="Scheduled runs are paused for every workspace"
          >
            Paused
          </span>
        )}
      </div>

      <div className={styles.railActions}>
        <button
          type="button"
          className={styles.newButton}
          onClick={onCreate}
          title="New workspace"
          aria-label="New workspace"
        >
          {collapsed ? '+' : '＋ New'}
        </button>
        <button
          type="button"
          className={`${styles.pauseAllButton} ${schedulerPaused ? styles.pauseAllButtonActive : ''}`}
          onClick={onToggleSchedulerPaused}
          disabled={schedulerPauseBusy}
          title={
            schedulerPaused
              ? 'Resume all scheduled runs'
              : 'Pause all scheduled runs across every workspace'
          }
          aria-label={schedulerPaused ? 'Resume all scheduled runs' : 'Pause all scheduled runs'}
        >
          {schedulerPaused ? <RunIcon /> : <PauseIcon />}
          {!collapsed && <span>{schedulerPaused ? 'Resume all' : 'Pause all'}</span>}
        </button>
      </div>

      {!collapsed && workspaces.length > 0 && (
        <div className={styles.filterRow}>
          <input
            type="text"
            className={styles.filterInput}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Filter workspaces…"
            aria-label="Filter workspaces by name"
            spellCheck={false}
            autoComplete="off"
          />
          {query && (
            <button
              type="button"
              className={styles.filterClear}
              onClick={() => setQuery('')}
              title="Clear filter"
              aria-label="Clear filter"
            >
              ×
            </button>
          )}
        </div>
      )}

      <div className={styles.railList}>
        {sections.map((section, index) => (
          <React.Fragment key={section.key}>
            {!collapsed && showHeaders && (
              <div className={styles.sectionHeader} role="presentation">
                {section.key === 'starred' && (
                  <span className={styles.sectionHeaderIcon} aria-hidden="true">
                    <StarIcon filled />
                  </span>
                )}
                {section.label}
              </div>
            )}
            {collapsed && index > 0 && (
              <div className={styles.sectionDivider} aria-hidden="true" />
            )}
            {section.items.map(renderRow)}
          </React.Fragment>
        ))}

        {workspaces.length === 0 && !collapsed && (
          <div className={styles.emptyRail}>
            No workspaces yet. Click ＋ New to start.
          </div>
        )}

        {workspaces.length > 0 && visible.length === 0 && !collapsed && (
          <div className={styles.emptyRail}>No workspaces match “{query.trim()}”.</div>
        )}
      </div>
    </nav>
  );
};

export default WorkspaceRail;
