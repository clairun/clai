import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import ReactDOM from 'react-dom';
import type { WorkspaceListEntry } from '../../generated/bindings';
import {
  CARD_STATUS_LABEL,
  deriveCardStatus,
  num,
} from '../../fleet/workspaceStatus';
import { formatIntervalShort, formatScheduleDetail } from '../../utils/scheduleFormat';
import {
  fixedRightAlignedMenuStyle,
  sameFloatingMenuStyle,
} from '../../utils/floatingMenu';
import styles from './WorkspaceRail.module.css';
import { useFleetActivityStore } from '../../stores/fleetActivityStore';

interface WorkspaceRailProps {
  id?: string;
  /** App-update notice shown under Settings; it renders nothing when there is no update. */
  updateNotice?: React.ReactNode;
  /** Leave for the Fleet: the same action as Ctrl/Cmd+Shift+M. */
  onOpenFleet: () => void;
  /** App-wide settings, as opposed to `onSettings` for one workspace. */
  onOpenGlobalSettings: () => void;
  onOpenWindow: (id: string) => void;
  workspaces: WorkspaceListEntry[];
  selectedId: string | null;
  /** Per-workspace pending approval/path-grant count (merged). */
  attentionCounts: Record<string, number>;
  attentionRequest?: number;
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
  schedulerPauseReadFailed: boolean;
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
  (attentionCounts[ws.id] || 0) > 0;

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

const FleetIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <rect x="3" y="3" width="7" height="7" rx="1" />
    <rect x="14" y="3" width="7" height="7" rx="1" />
    <rect x="3" y="14" width="7" height="7" rx="1" />
    <rect x="14" y="14" width="7" height="7" rx="1" />
  </svg>
);

const SettingsIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
  </svg>
);

const PlusIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" aria-hidden="true">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

const RetryIcon = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <polyline points="23 4 23 10 17 10" />
    <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
  </svg>
);

const ClockIcon = () => (
  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <polyline points="12 7 12 12 15 14" />
  </svg>
);

/** The cadence in a few characters, for the row's trailing schedule cue:
 *  "2 h" or "90 min" for an interval, "cron" for a cron expression. The full
 *  label (`formatScheduleDetail`) goes to the tooltip and to screen readers. */
const shortScheduleLabel = (kind: WorkspaceListEntry['scheduleKind']): string => {
  if (kind?.type === 'interval') return formatIntervalShort(Number(kind.intervalMinutes ?? 0)) ?? '';
  return kind?.type === 'cron' ? 'cron' : '';
};

const NewWindowIcon = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.25" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
    <polyline points="15 3 21 3 21 9" />
    <line x1="10" y1="14" x2="21" y2="3" />
  </svg>
);

const StarIcon = () => (
  <svg
    width="12"
    height="12"
    viewBox="0 0 24 24"
    fill="currentColor"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
  </svg>
);

/** Height reserved for the per-row ⋯ menu when deciding whether it fits
 *  below the trigger. Four items of 32px (12px text at line-height 1.5,
 *  plus 7px padding top and bottom), three 1px gaps, 4px of padding at each
 *  end and a 1px border each side ≈ 141px. Rounded up so the flip triggers
 *  a hair early, which costs nothing: it only ever picks the roomier side.
 *  Drift here is bounded — `maxHeight` caps the menu either way. */
const ROW_MENU_HEIGHT = 144;
const ROW_MENU_GAP = 4;
/** Minimum margin kept between the menu and the viewport edges. */
const ROW_MENU_MARGIN = 8;
/** Mirrors `min-width` on `.menu` — used to clamp the right offset. */
const ROW_MENU_MIN_WIDTH = 140;

/**
 * Viewport-fixed position for a row's ⋯ menu, anchored to its trigger.
 *
 * The menu is portaled to `document.body` instead of being absolutely
 * positioned inside the row, because the row lives in `.railList`, which
 * scrolls (`overflow-y: auto`): on the last rows an in-row menu was clipped
 * out of sight, and since it still extended the list's scrollable area it
 * produced a scrollbar that could not be used — dragging it hit the
 * click-away backdrop, which closed the menu. The collapsed rail clipped it
 * horizontally too (`.rail` is `overflow: hidden`).
 *
 * Right-aligned with the trigger; below it when there is room, flipped
 * above when there is not, and capped to the space on whichever side it
 * ends up on so a short window scrolls the menu instead of cutting it off.
 */
const rowMenuStyle = (trigger: HTMLElement): React.CSSProperties => {
  return fixedRightAlignedMenuStyle(trigger, {
    estimatedHeight: ROW_MENU_HEIGHT,
    gap: ROW_MENU_GAP,
    margin: ROW_MENU_MARGIN,
    minWidth: ROW_MENU_MIN_WIDTH,
  });
};

/**
 * Persistent left navigator for the unified Fleet/Workspace view. Lists
 * every workspace grouped into explicit, labeled sections (Claude-style)
 * so ordering is never a mystery:
 *
 *   1. "Needs attention" — open questions, pending approvals and path grants.
 *   2. "Starred" — user-pinned workspaces (star via the ⋯ menu).
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
  id,
  updateNotice,
  onOpenFleet,
  onOpenGlobalSettings,
  onOpenWindow,
  attentionCounts,
  attentionRequest = 0,
  activeRuns,
  collapsed,
  onToggleCollapsed,
  onSelect,
  onCreate,
  schedulerPaused,
  schedulerPauseReadFailed,
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
  // Which row's ⋯ menu is open, and where it is pinned. The position is
  // recomputed from the live trigger after every render (see below), so a
  // row that moves takes its menu with it.
  const [openMenu, setOpenMenu] = useState<{
    id: string;
    style: React.CSSProperties;
  } | null>(null);
  const menuTriggerRef = useRef<HTMLElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const openMenuId = openMenu?.id ?? null;
  const runNowSkipped = useFleetActivityStore((state) => state.runNowSkipped);

  // Opening the menu takes focus into it, so closing it hands focus back to
  // the ⋯ trigger — otherwise the focused element just disappears, focus
  // lands on <body> and the next Tab restarts from the top of the document.
  // Only when focus is actually inside the menu, so a close triggered while
  // the user is somewhere else never steals it. Focus first, then close:
  // while the menu is open its row keeps the actions laid out, so the
  // trigger is still focusable at this point.
  const closeMenu = useCallback(() => {
    if (menuRef.current?.contains(document.activeElement)) {
      menuTriggerRef.current?.focus();
    }
    setOpenMenu(null);
  }, []);
  const [filter, setFilter] = useState({ query: '', request: attentionRequest });
  const query = filter.request === attentionRequest ? filter.query : '';
  const selectedIdRef = useRef(selectedId);
  useLayoutEffect(() => {
    selectedIdRef.current = selectedId;
  }, [selectedId]);
  useLayoutEffect(() => {
    if (attentionRequest === 0) return;
    const frame = window.requestAnimationFrame(() => {
      const rail = document.getElementById(id ?? '');
      const rows = rail?.querySelectorAll<HTMLElement>('[data-attention]') ?? [];
      Array.from(rows).find((row) => row.dataset.workspaceId !== selectedIdRef.current)?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [attentionRequest, id]);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);

  // The menu lives at the end of <body>, so Tab from the ⋯ trigger no
  // longer reaches it. Move focus onto it on open, and hand focus back to
  // the trigger when Escape closes it.
  useEffect(() => {
    if (!openMenuId) return;
    menuRef.current
      ?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')
      ?.focus();
  }, [openMenuId]);

  // A viewport-fixed menu cannot follow a scrolling row, and re-anchoring on
  // resize would fight the layout mid-drag, so both dismiss it — as does
  // Escape, which the click-away backdrop cannot cover for keyboard users.
  useEffect(() => {
    if (!openMenuId) return;
    const close = () => closeMenu();
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      closeMenu();
    };
    // Capture, because scroll does not bubble — but only scrollers that
    // actually carry the trigger count. The app scrolls other panes
    // programmatically while a run streams (see VirtualizedList), and those
    // must not yank the menu shut.
    const onScroll = (e: Event) => {
      const trigger = menuTriggerRef.current;
      const target = e.target;
      if (!trigger || target === document || !(target instanceof Node)) {
        close();
        return;
      }
      if (target.contains(trigger)) close();
    };
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('resize', close);
    window.addEventListener('scroll', onScroll, true);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('resize', close);
      window.removeEventListener('scroll', onScroll, true);
    };
  }, [openMenuId, closeMenu]);

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

  // Rows move when the 5s workspace poll re-sorts them or a section appears,
  // so re-anchor to the live trigger instead of leaving the menu at
  // coordinates its row has left — and close if the row left the list
  // entirely. Runs on every change that can reorder rows; it only settles
  // into a state update when the trigger actually moved.
  useLayoutEffect(() => {
    const trigger = menuTriggerRef.current;
    if (!openMenu || !trigger) return;
    if (!trigger.isConnected) {
      setOpenMenu(null);
      return;
    }
    // An unmeasurable trigger (zero box) would re-anchor the menu to the
    // viewport corner. Keep the last good position instead.
    const rect = trigger.getBoundingClientRect();
    if (!rect.width && !rect.height) return;
    const style = rowMenuStyle(trigger);
    if (!sameFloatingMenuStyle(style, openMenu.style)) {
      setOpenMenu({ id: openMenu.id, style });
    }
  }, [openMenu, sections, collapsed]);

  const renderRow = (ws: WorkspaceListEntry) => {
    const processing = isProcessing(ws, activeRuns);
    const pending = attentionCounts[ws.id] || 0;
    const status = deriveCardStatus(ws, processing, pending > 0);
    const isSelected = ws.id === selectedId;
    const scheduleLabel = formatScheduleDetail(ws.scheduleKind);
    const isPaused = !!ws.schedulePaused;
    const scheduleText = isPaused
      ? `Paused${scheduleLabel ? ` · ${scheduleLabel}` : ''}`
      : scheduleLabel || 'Scheduled';
    const isStarred = !!ws.starred;
    const attentionCount = pending;
    // A run completed since the user last opened this workspace.
    // Suppressed while selected — the open page is marking it seen.
    const isUnread = !!ws.unread && !isSelected;
    const initial = (ws.title || '?').trim().charAt(0).toUpperCase() || '?';
    const isRunNowSkipped = runNowSkipped.includes(ws.id);
    const rowClasses = [
      styles.row,
      isSelected ? styles.rowSelected : '',
      isUnread ? styles.rowUnread : '',
      // Sizes the hover-action overlay: running schedules add Run now and
      // Pause, paused ones add Resume.
      ws.scheduleEnabled ? (isPaused ? styles.rowScheduledPaused : styles.rowScheduled) : '',
      isRunNowSkipped ? styles.rowSkipped : '',
      // An open menu keeps its row's actions visible. Opening the menu moves
      // focus into it, and it is portaled to <body>, so the row loses
      // `:focus-within` (and `:hover` as soon as the pointer moves onto the
      // menu).
      openMenu?.id === ws.id ? styles.rowMenuOpen : '',
    ]
      .filter(Boolean)
      .join(' ');

    return (
      <div
        key={ws.id}
        data-workspace-id={ws.id}
        data-attention={attentionCount > 0 || undefined}
        className={`${rowClasses}${
          dropTargetId === ws.id ? ` ${styles.rowDropTarget}` : ''
        }`}
        onClick={() => onSelect(ws.id)}
        role="button"
        tabIndex={0}
        aria-current={isSelected || undefined}
        onKeyDown={(e) => {
          // The ⋯ menu is portaled, but portals still bubble through the
          // React tree — so Enter on a menu item (or on any row button)
          // would select the workspace as well as run the action.
          if (e.target !== e.currentTarget) return;
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
            </span>

            {isRunNowSkipped ? (
              <span
                className={styles.rowSkipNotice}
                role="status"
                title="Run now skipped: session busy"
              >
                <span aria-hidden="true">Busy</span>
                <span className="sr-only">Run now skipped: session busy</span>
              </span>
            ) : ws.scheduleEnabled ? (
              <span
                className={`${styles.rowMeta} ${isPaused ? styles.rowMetaPaused : ''}`}
                title={scheduleText}
              >
                {isPaused ? <PauseIcon /> : <ClockIcon />}
                <span aria-hidden="true">{shortScheduleLabel(ws.scheduleKind)}</span>
                <span className="sr-only">{scheduleText}</span>
              </span>
            ) : null}

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
              {ws.scheduleEnabled && !isPaused && (
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
                      onTogglePause(ws.id, false);
                    }}
                    disabled={pauseBusyId === ws.id}
                    title="Pause schedule"
                    aria-label="Pause schedule"
                  >
                    <PauseIcon />
                  </button>
                </>
              )}
              {ws.scheduleEnabled && isPaused && (
                <button
                  type="button"
                  className={styles.iconButton}
                  onClick={(e) => {
                    e.stopPropagation();
                    onTogglePause(ws.id, true);
                  }}
                  disabled={pauseBusyId === ws.id}
                  title="Resume schedule"
                  aria-label="Resume schedule"
                >
                  <RunIcon />
                </button>
              )}
              <button
                type="button"
                className={styles.iconButton}
                onClick={(e) => {
                  e.stopPropagation();
                  onOpenWindow(ws.id);
                }}
                title="Open in new window"
                aria-label={`Open ${ws.title} in new window`}
              >
                <NewWindowIcon />
              </button>
              <button
                type="button"
                className={styles.iconButton}
                onClick={(e) => {
                  e.stopPropagation();
                  if (openMenu?.id === ws.id) {
                    setOpenMenu(null);
                    return;
                  }
                  const trigger = e.currentTarget;
                  menuTriggerRef.current = trigger;
                  setOpenMenu({ id: ws.id, style: rowMenuStyle(trigger) });
                }}
                title="More actions"
                aria-label="More actions"
                aria-haspopup="menu"
                aria-expanded={openMenu?.id === ws.id}
              >
                ⋯
              </button>
            </span>

            {openMenu?.id === ws.id &&
              ReactDOM.createPortal(
                <>
                  <button
                    type="button"
                    className={styles.menuBackdrop}
                    aria-hidden="true"
                    tabIndex={-1}
                    // Clicking the backdrop must not pull focus out of the
                    // menu first — `closeMenu` only hands focus back to the
                    // trigger when it still holds it.
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={(e) => {
                      e.stopPropagation();
                      closeMenu();
                    }}
                  />
                  <div
                    className={styles.menu}
                    role="menu"
                    ref={menuRef}
                    style={openMenu.style}
                    onKeyDown={(e) => {
                      // Tab walks the items, as it did when the menu lived
                      // in the row. Only a Tab that would leave the menu is
                      // intercepted: the portal is the last thing in
                      // <body>, so letting focus out would land nowhere
                      // useful and strand the menu behind its
                      // click-swallowing backdrop. Dismiss and hand focus
                      // back to the trigger instead, so the next Tab
                      // continues from the row.
                      if (e.code !== 'Tab') return;
                      const items = Array.from(
                        e.currentTarget.querySelectorAll<HTMLElement>(
                          '[role="menuitem"]:not([disabled])',
                        ),
                      );
                      const at = items.indexOf(document.activeElement as HTMLElement);
                      const leaving = e.shiftKey ? at <= 0 : at < 0 || at >= items.length - 1;
                      if (!leaving) return;
                      e.preventDefault();
                      closeMenu();
                    }}
                  >
                    <button
                      type="button"
                      className={styles.menuItem}
                      role="menuitem"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeMenu();
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
                        closeMenu();
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
                        closeMenu();
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
                        closeMenu();
                        onDelete(ws.id, ws.title);
                      }}
                    >
                      Delete
                    </button>
                  </div>
                </>,
                document.body,
              )}
          </>
        )}
      </div>
    );
  };

  return (
    <nav
      id={id}
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
        <button
          type="button"
          className={styles.newButton}
          onClick={onCreate}
          title="New workspace"
          aria-label="New workspace"
        >
          <PlusIcon />
          {!collapsed && <span>New workspace</span>}
        </button>
      </div>

      {/* Destinations outside this list. Above it, not below: under a long
          scrolling list they were out of sight. */}
      <div className={styles.railNav}>
        <button
          type="button"
          className={styles.navItem}
          onClick={() => onOpenFleet()}
          aria-label="Open Fleet"
          aria-keyshortcuts="Control+Shift+M Meta+Shift+M"
          title="Open Fleet (Ctrl/Cmd+Shift+M)"
        >
          <FleetIcon />
          {!collapsed && <span>Fleet</span>}
        </button>
        <button
          type="button"
          className={styles.navItem}
          onClick={() => onOpenGlobalSettings()}
          aria-label="Open settings"
          title="Settings"
        >
          <SettingsIcon />
          {!collapsed && <span>Settings</span>}
        </button>
        {updateNotice && <div className={styles.updateNotice}>{updateNotice}</div>}
      </div>

      <div className={styles.listHeader}>
        {!collapsed && <span className={styles.listTitle}>Workspaces</span>}
        {!collapsed && <span className={styles.listCount}>{workspaces.length}</span>}
        <button
          type="button"
          className={`${styles.pauseAllButton} ${schedulerPaused && !schedulerPauseReadFailed ? styles.pauseAllButtonActive : ''}`}
          onClick={onToggleSchedulerPaused}
          disabled={schedulerPauseBusy}
          title={
            schedulerPauseReadFailed ? 'Could not read global pause state. Retry' : schedulerPaused
              ? 'Scheduled runs are paused for every workspace. Resume all'
              : 'Pause all scheduled runs across every workspace'
          }
          aria-label={schedulerPauseReadFailed ? 'Retry reading global pause state' : schedulerPaused ? 'Resume all scheduled runs' : 'Pause all scheduled runs'}
        >
          {schedulerPauseReadFailed ? <RetryIcon /> : schedulerPaused ? <RunIcon /> : <PauseIcon />}
          {!collapsed && <span>{schedulerPauseReadFailed ? 'Retry' : schedulerPaused ? 'Resume all' : 'Pause all'}</span>}
        </button>
      </div>

      {!collapsed && workspaces.length > 0 && (
        <div className={styles.filterRow}>
          <input
            type="text"
            className={styles.filterInput}
            value={query}
            onChange={(e) => setFilter({ query: e.target.value, request: attentionRequest })}
            placeholder="Filter workspaces…"
            aria-label="Filter workspaces by name"
            spellCheck={false}
            autoComplete="off"
          />
          {query && (
            <button
              type="button"
              className={styles.filterClear}
              onClick={() => setFilter({ query: '', request: attentionRequest })}
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
                    <StarIcon />
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
            No workspaces yet. Click New workspace to start.
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
