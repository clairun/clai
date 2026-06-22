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

/**
 * Persistent left navigator for the unified Fleet/Workspace view. Lists
 * every workspace; clicking a row selects it (the host navigates to
 * `/workspace/:id`). Attention workspaces (pending approval, failed or
 * blocked tasks) are pinned to the top with a badge so cross-workspace
 * issues surface without a separate panel.
 *
 * Collapsible: the collapsed state shows just a status dot + initial,
 * with the full title on hover (title attr). The host owns the collapsed
 * flag (persisted to localStorage) and all data/actions; this component
 * is presentational plus a small amount of per-row menu state.
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
  onSettings,
  onFork,
  onDelete,
  runNowBusyId,
  forkBusyId,
  pauseBusyId,
}: WorkspaceRailProps) => {
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [query, setQuery] = useState('');

  // Sort: attention first, then scheduled, then most-recently-updated.
  const sorted = useMemo(
    () =>
      [...workspaces].sort((a, b) => {
        const aAtt = hasAttention(a, attentionCounts);
        const bAtt = hasAttention(b, attentionCounts);
        if (aAtt !== bAtt) return aAtt ? -1 : 1;
        const aSched = !!a.scheduleEnabled;
        const bSched = !!b.scheduleEnabled;
        if (aSched !== bSched) return aSched ? -1 : 1;
        return num(b.updatedAt) - num(a.updatedAt);
      }),
    [workspaces, attentionCounts],
  );

  // Name filter. Applied only when expanded — collapsed has no input, so
  // it shows the full list. Case-insensitive substring match on title.
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (collapsed || !q) return sorted;
    return sorted.filter((ws) => (ws.title || '').toLowerCase().includes(q));
  }, [sorted, query, collapsed]);

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
        {visible.map((ws) => {
          const processing = isProcessing(ws, activeRuns);
          const pending = attentionCounts[ws.id] || 0;
          const status = deriveCardStatus(ws, processing, pending > 0);
          const isSelected = ws.id === selectedId;
          const scheduleLabel = formatScheduleLabel(ws.scheduleKind);
          const isPaused = !!ws.schedulePaused;
          const attentionCount = pending + num(ws.failedTaskCount) + num(ws.blockedTaskCount);
          // A run completed since the user last opened this workspace.
          // Suppressed while selected — the open page is marking it seen.
          const isUnread = !!ws.unread && !isSelected;
          const initial = (ws.title || '?').trim().charAt(0).toUpperCase() || '?';
          const rowClasses = [styles.row, isSelected ? styles.rowSelected : ''].join(' ');

          return (
            <div
              key={ws.id}
              className={rowClasses}
              onClick={() => onSelect(ws.id)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === 'Enter') onSelect(ws.id);
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
        })}

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
