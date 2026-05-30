import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Outlet, useMatch, useNavigate } from 'react-router-dom';
import {
  listWorkspaces,
  deleteWorkspace,
  cloneWorkspaceConfig,
  getWorkspaceSnapshot,
  runWorkspaceNow,
  setWorkspaceSchedulePaused,
  createWorkspace,
} from '../workspace/client';
import WorkspaceRail from '../components/Fleet/WorkspaceRail';
import WorkspaceSettingsModal from '../components/Settings/WorkspaceSettingsModal';
import { SettingsModal } from '../components/Settings';
import ConfirmDialog from '../components/ConfirmDialog';
import { useFleetActivity } from '../hooks/useFleetActivity';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { usePermissionAttention } from '../hooks/usePermissionAttention';
import { errText, num } from '../fleet/workspaceStatus';
import type { WorkspaceListEntry, WorkspaceSnapshot } from '../generated/bindings';
import styles from './FleetLayout.module.css';

const REFRESH_INTERVAL_MS = 5000;
const OPTIMISTIC_RUN_TTL_MS = 12000;
const COLLAPSED_KEY = 'fleet.rail.collapsed';

/**
 * Shared via `<Outlet context={...}>` so the nested Workspace view can read
 * the rail's workspace list and trigger an immediate refresh (e.g. after an
 * inline title rename) instead of waiting for the 5s poll.
 */
export interface FleetOutletContext {
  workspaces: WorkspaceListEntry[];
  loadWorkspaces: () => Promise<void>;
}

interface SettingsState {
  open: boolean;
  workspaceId: string | null;
  snapshot: WorkspaceSnapshot | null;
}

interface PendingDelete {
  id: string;
  title: string;
}

/**
 * Unified Fleet/Workspace shell: a persistent (collapsible) workspace
 * rail on the left and the selected workspace's full view in the
 * `<Outlet>` on the right. Replaces the old standalone Fleet card grid —
 * the rail is the navigator, and the main area is the real Workspace
 * component (rendered by the `/workspace/:id` route nested under this
 * layout).
 *
 * This layout owns the cross-workspace concerns the grid used to: the
 * workspace list + polling, the summary counters, and the create / clone /
 * delete / settings / run / pause actions plus their modals.
 */
const FleetLayout = () => {
  const navigate = useNavigate();
  const match = useMatch('/workspace/:workspaceId');
  const selectedId = match?.params.workspaceId ?? null;

  const [workspaces, setWorkspaces] = useState<WorkspaceListEntry[]>([]);
  const [error, setError] = useState('');
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      return localStorage.getItem(COLLAPSED_KEY) === '1';
    } catch {
      return false;
    }
  });

  const [settingsState, setSettingsState] = useState<SettingsState>({
    open: false,
    workspaceId: null,
    snapshot: null,
  });
  const [pendingDelete, setPendingDelete] = useState<PendingDelete | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [globalSettingsOpen, setGlobalSettingsOpen] = useState(false);
  const [cloneBusyId, setCloneBusyId] = useState<string | null>(null);
  const [runNowBusyId, setRunNowBusyId] = useState<string | null>(null);
  const [pauseBusyId, setPauseBusyId] = useState<string | null>(null);

  const activeRunsByWorkspace = useFleetActivity() as Record<string, number>;
  const pendingPermissionCounts = usePermissionAttention() as Record<string, number>;

  const loadWorkspaces = useCallback(async () => {
    try {
      const all = await listWorkspaces();
      setWorkspaces(all || []);
      setError('');
    } catch (err) {
      setError(errText(err, 'Failed to load workspaces.'));
    }
  }, []);

  useEffect(() => {
    loadWorkspaces();
    const interval = window.setInterval(loadWorkspaces, REFRESH_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, [loadWorkspaces]);

  // Publish the rail's current width so the globally-fixed terminal card (in
  // MainLayout, a sibling of our Outlet) can center over the detail pane rather
  // than the whole viewport. Values mirror `.rail` / `.railCollapsed` in
  // WorkspaceRail.module.css. Cleared on unmount so non-Fleet routes fall back
  // to viewport-centered.
  useEffect(() => {
    const railWidth = collapsed ? '52px' : '248px';
    document.documentElement.style.setProperty('--fleet-rail-width', railWidth);
    return () => {
      document.documentElement.style.removeProperty('--fleet-rail-width');
    };
  }, [collapsed]);

  const toggleCollapsed = useCallback(() => {
    setCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(COLLAPSED_KEY, next ? '1' : '0');
      } catch {
        /* ignore persistence failure */
      }
      return next;
    });
  }, []);

  const counters = useMemo(
    () => ({
      total: workspaces.length,
      periodic: workspaces.filter((w) => w.scheduleEnabled).length,
      running: workspaces.filter(
        (w) => num(w.runningTaskCount) > 0 || (activeRunsByWorkspace[w.id] || 0) > 0,
      ).length,
      attention: workspaces.filter(
        (w) =>
          num(w.attentionTaskCount) > 0 || (pendingPermissionCounts[w.id] || 0) > 0,
      ).length,
    }),
    [workspaces, activeRunsByWorkspace, pendingPermissionCounts],
  );

  const handleSelect = useCallback(
    (id: string) => {
      navigate(`/workspace/${id}`);
    },
    [navigate],
  );

  const handleCreate = useCallback(async () => {
    try {
      const id = await createWorkspace();
      await loadWorkspaces();
      navigate(`/workspace/${id}`);
    } catch (err) {
      setError(errText(err, 'Failed to create workspace.'));
    }
  }, [navigate, loadWorkspaces]);

  const handleOpenSettings = useCallback(async (id: string) => {
    if (!id) return;
    try {
      const snapshot = await getWorkspaceSnapshot(id);
      setSettingsState({ open: true, workspaceId: id, snapshot });
    } catch (err) {
      setError(errText(err, 'Failed to open workspace settings.'));
    }
  }, []);

  const handleSettingsClose = useCallback(() => {
    setSettingsState({ open: false, workspaceId: null, snapshot: null });
  }, []);

  const handleSettingsChanged = useCallback(async () => {
    const id = settingsState.workspaceId;
    if (!id) return;
    try {
      const snapshot = await getWorkspaceSnapshot(id);
      setSettingsState((s) => (s.workspaceId === id ? { ...s, snapshot } : s));
    } catch {
      /* non-fatal — modal stays open with old snapshot */
    }
    loadWorkspaces();
  }, [settingsState.workspaceId, loadWorkspaces]);

  const handleClone = useCallback(
    async (id: string) => {
      if (!id || cloneBusyId) return;
      setCloneBusyId(id);
      try {
        const newId = await cloneWorkspaceConfig(id);
        setError('');
        await loadWorkspaces();
        navigate(`/workspace/${newId}`);
      } catch (err) {
        setError(errText(err, 'Failed to clone workspace.'));
      } finally {
        setCloneBusyId(null);
      }
    },
    [cloneBusyId, loadWorkspaces, navigate],
  );

  const handleRequestDelete = useCallback((id: string, title?: string) => {
    if (!id) return;
    setPendingDelete({ id, title: title || 'this workspace' });
  }, []);

  const handleCancelDelete = useCallback(() => {
    if (deleting) return;
    setPendingDelete(null);
  }, [deleting]);

  const handleConfirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    const { id } = pendingDelete;
    setDeleting(true);
    try {
      await deleteWorkspace(id);
      await loadWorkspaces();
      setPendingDelete(null);
      // If we just deleted the open workspace, fall back to /fleet so the
      // index can re-pick a most-recent target (or show the empty state).
      if (selectedId === id) {
        navigate('/fleet', { replace: true });
      }
    } catch (err) {
      setError(errText(err, 'Failed to delete workspace.'));
    } finally {
      setDeleting(false);
    }
  }, [pendingDelete, loadWorkspaces, selectedId, navigate]);

  const handleRunNow = useCallback(
    async (id: string) => {
      if (!id || runNowBusyId) return;
      setRunNowBusyId(id);
      try {
        await runWorkspaceNow(id);
        setError('');
        const fleet = useFleetActivityStore.getState();
        const optimisticId = `optimistic-runnow:${id}`;
        fleet.markRunStarted(id, optimisticId);
        setTimeout(() => fleet.markRunEnded(id, optimisticId), OPTIMISTIC_RUN_TTL_MS);
      } catch (err) {
        setError(errText(err, 'Failed to start run.'));
      } finally {
        setRunNowBusyId(null);
      }
    },
    [runNowBusyId],
  );

  const handleTogglePause = useCallback(
    async (id: string, currentlyPaused: boolean) => {
      if (!id || pauseBusyId) return;
      setPauseBusyId(id);
      const nextPaused = !currentlyPaused;
      setWorkspaces((prev) =>
        prev.map((w) => (w.id === id ? { ...w, schedulePaused: nextPaused } : w)),
      );
      try {
        await setWorkspaceSchedulePaused(id, nextPaused);
        setError('');
        await loadWorkspaces();
      } catch (err) {
        setError(errText(err, 'Failed to update pause state.'));
        setWorkspaces((prev) =>
          prev.map((w) => (w.id === id ? { ...w, schedulePaused: currentlyPaused } : w)),
        );
      } finally {
        setPauseBusyId(null);
      }
    },
    [pauseBusyId, loadWorkspaces],
  );

  return (
    <div className={styles.layout}>
      <div className={styles.topBar}>
        <img src="/icon.svg" alt="Clai" className={styles.brandIcon} />
        <div className={styles.counters} role="status" aria-label="Fleet summary">
          <span className={styles.counterChip}>
            <strong>{counters.total}</strong> workspace{counters.total === 1 ? '' : 's'}
          </span>
          <span className={styles.counterSep}>·</span>
          <span className={styles.counterChip}>
            <strong>{counters.periodic}</strong> periodic
          </span>
          <span className={styles.counterSep}>·</span>
          <span className={styles.counterChip}>
            <strong>{counters.running}</strong> running
          </span>
          <span className={styles.counterSep}>·</span>
          <span
            className={`${styles.counterChip} ${counters.attention > 0 ? styles.counterChipAttention : ''}`}
          >
            <strong>{counters.attention}</strong> need attention
          </span>
        </div>
        <button
          type="button"
          className={styles.settingsButton}
          onClick={() => setGlobalSettingsOpen(true)}
          title="Settings"
          aria-label="Open settings"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={styles.body}>
        <WorkspaceRail
          workspaces={workspaces}
          selectedId={selectedId}
          attentionCounts={pendingPermissionCounts}
          activeRuns={activeRunsByWorkspace}
          collapsed={collapsed}
          onToggleCollapsed={toggleCollapsed}
          onSelect={handleSelect}
          onCreate={handleCreate}
          onRunNow={handleRunNow}
          onTogglePause={handleTogglePause}
          onSettings={handleOpenSettings}
          onClone={handleClone}
          onDelete={handleRequestDelete}
          runNowBusyId={runNowBusyId}
          cloneBusyId={cloneBusyId}
          pauseBusyId={pauseBusyId}
        />
        <div className={styles.detail}>
          <Outlet context={{ workspaces, loadWorkspaces }} />
        </div>
      </div>

      <WorkspaceSettingsModal
        isOpen={settingsState.open}
        onClose={handleSettingsClose}
        workspaceId={settingsState.workspaceId || ''}
        snapshot={settingsState.snapshot}
        initialSelection={{ kind: 'general' }}
        onChanged={handleSettingsChanged}
      />

      <SettingsModal
        isOpen={globalSettingsOpen}
        onClose={() => setGlobalSettingsOpen(false)}
      />

      <ConfirmDialog
        isOpen={!!pendingDelete}
        title="Delete workspace?"
        body={(
          <>
            <strong>{pendingDelete?.title}</strong> will be permanently deleted, along
            with its agents, chat history, schedules, and artifacts. This cannot be
            undone.
          </>
        )}
        confirmLabel="Delete workspace"
        cancelLabel="Cancel"
        confirmTone="danger"
        busy={deleting}
        onCancel={handleCancelDelete}
        onConfirm={handleConfirmDelete}
      />
    </div>
  );
};

export default FleetLayout;
