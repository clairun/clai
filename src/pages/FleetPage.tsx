import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router';
import AppTopBar from '../components/AppTopBar';
import { AGENT_QUESTION } from '../components/AskUserPanel/AskUserPanel';
import FleetGrid from '../components/FleetPage/FleetGrid';
import FleetSwitcher from '../components/FleetPage/FleetSwitcher';
import styles from '../components/FleetPage/FleetPage.module.css';
import { listAgentDefinitions } from '../api/client';
import { identityFor, type AgentIdentity } from '../components/Agents/agentIdentity';
import { RECENT_WINDOW_MS, cardPhase, type FleetLive } from '../fleet/fleetModel';
import { eligibleCards, recentlyOpened, undismissed } from '../fleet/boardSlots';
import { useFleetStore } from '../fleet/fleetStore';
import { useMainRunning } from '../fleet/useMainRunning';
import { useMinute } from '../fleet/useMinute';
import { useFleetToggle } from '../fleet/useFleetToggle';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import { useVisibleWorkspaceRead } from '../hooks/useVisibleWorkspaceRead';
import { openWorkspaceWindow } from '../windowUi';

const RECENT_HOURS = RECENT_WINDOW_MS / 3_600_000;

/**
 * Where Enter does its own thing, so it doesn't open the selected workspace.
 * A card's title is a button only to select the card, so Enter there opens;
 * so it does in the terminal's composer while it leaves Enter to the page.
 */
const OWNS_ENTER =
  'input, textarea:not([data-leaves-enter]), select, a, button:not([data-card-title]), [contenteditable="true"], [role="dialog"]';
/** A zoom or a modal; an agent question sits inline in its card. */
const OPEN_DIALOG = `[role="dialog"]:not(${AGENT_QUESTION})`;

/**
 * The Fleet: one card per recently active workspace, with the shared
 * terminal at the bottom. Clicking a card selects it: it is highlighted and
 * the terminal talks to it; with nothing selected the terminal starts a new
 * workspace.
 */
const FleetPage = () => {
  const [newWindowError, setNewWindowError] = useState('');
  const openNewWindow = useCallback((id: string) => {
    setNewWindowError('');
    void openWorkspaceWindow(id).catch((err) => setNewWindowError(err instanceof Error ? err.message : 'Failed to open a new window.'));
  }, []);
  const cards = useFleetStore((s) => s.fleet.cards);
  const now = useFleetStore((s) => s.now);
  // Panels are judged against this: freshness, shouts and captions age on the
  // minute tick, while what a panel says changes with events.
  const clock = Math.max(now, useMinute());
  const selectedId = useFleetStore((s) => s.selectedId);
  const select = useFleetStore((s) => s.select);
  const dismiss = useFleetStore((s) => s.dismiss);
  const dismissed = useFleetStore((s) => s.dismissed);
  const loadedAt = useFleetStore((s) => s.loadedAt);
  const loadError = useFleetStore((s) => s.loadError);
  const runs = useFleetActivityStore((s) => s.activeRunsByWorkspace);
  const tasks = useTaskStore((s) => s.tasks);
  const live: FleetLive = useMemo(() => ({ runs, tasks }), [runs, tasks]);

  // Reload on mount and whenever the window regains focus: that re-judges
  // recency without a timer, and drops cards of deleted workspaces. The
  // agent library gives the cards' faces their picked look.
  const [faces, setFaces] = useState<Record<string, AgentIdentity>>({});
  useEffect(() => {
    const load = () => {
      void useFleetStore.getState().loadSnapshot();
      listAgentDefinitions()
        .then((definitions) =>
          setFaces(Object.fromEntries(definitions.map((d) => [d.id, identityFor({ id: d.id, avatar: d.avatar })])))
        )
        .catch(() => {
          // Faces fall back to the ones their ids map to.
        });
    };
    load();
    window.addEventListener('focus', load);
    return () => window.removeEventListener('focus', load);
  }, []);

  useEffect(() => {
    const refresh = () => useFleetStore.getState().refreshDismissed();
    const onStorage = (event: StorageEvent) => {
      if (event.key === 'fleet.dismissed' || event.key === null) refresh();
    };
    refresh();
    window.addEventListener('storage', onStorage);
    window.addEventListener('focus', refresh);
    return () => {
      window.removeEventListener('storage', onStorage);
      window.removeEventListener('focus', refresh);
    };
  }, []);

  // The store's clock, not `clock`: a card leaves the board on an event, not on the minute tick.
  const placed = useMemo(
    () => eligibleCards(Object.values(cards), { now, selectedId, live }),
    [cards, now, selectedId, live]
  );
  // The switcher still lists a dismissed card; the board doesn't.
  const onBoard = useMemo(() => undismissed(placed, cards, dismissed), [placed, cards, dismissed]);
  const offBoard = useMemo(() => new Set(placed.filter((id) => !onBoard.includes(id))), [placed, onBoard]);

  // Escape deselects, unless something else used it first (the switcher,
  // the terminal clearing its output, or an open dialog). Agent questions
  // sit inline in the cards; they don't hold Escape.
  useEffect(() => {
    const onEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !e.defaultPrevented && !document.querySelector(OPEN_DIALOG)) select(null);
    };
    window.addEventListener('keydown', onEscape);
    return () => window.removeEventListener('keydown', onEscape);
  }, [select]);

  const selectedWorking = useMainRunning(selectedId);
  useVisibleWorkspaceRead(selectedId, selectedWorking);

  // Cards born from live events (or a workspace just created here) have no
  // title yet: the snapshot has it, once per new set of untitled cards.
  const untitled = placed.filter((id) => !cards[id]!.title).join(',');
  useEffect(() => {
    if (untitled) void useFleetStore.getState().loadSnapshot();
  }, [untitled]);

  // Every card counts, placed or not, so waiting work that doesn't fit still shows here.
  const counts = useMemo(() => {
    let working = 0;
    let waiting = 0;
    for (const card of Object.values(cards)) {
      const phase = cardPhase(card, live);
      if (phase === 'working') working += 1;
      if (phase === 'waiting') waiting += 1;
    }
    return { working, waiting };
  }, [cards, live]);

  // Opening a workspace here: Enter, a double-click on a card's title, or
  // Ctrl/Cmd+Shift+M, which opens the selected card's workspace, or the
  // workspaces page without one. The card's icon and Shift+Enter open it in a
  // new window instead.
  const navigate = useNavigate();
  const openWorkspace = useCallback((id: string) => navigate(`/workspace/${id}`), [navigate]);
  useFleetToggle(useCallback(() => {
    if (selectedId) openWorkspace(selectedId);
    else navigate('/workspace');
  }, [selectedId, openWorkspace, navigate]));
  useEffect(() => {
    if (!selectedId) return undefined;
    const onEnter = (e: KeyboardEvent) => {
      if (e.key !== 'Enter' || e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target instanceof Element ? e.target : null;
      // The composer left this Enter to the page: it is taken even when it opens nothing, so no newline lands there.
      if (target?.matches('[data-leaves-enter]')) e.preventDefault();
      if (e.repeat || target?.closest(OWNS_ENTER) || document.querySelector(OPEN_DIALOG)) return;
      e.preventDefault();
      if (e.shiftKey) openNewWindow(selectedId);
      else openWorkspace(selectedId);
    };
    window.addEventListener('keydown', onEnter);
    return () => window.removeEventListener('keydown', onEnter);
  }, [selectedId, openWorkspace, openNewWindow]);

  const switcherOrder = useMemo(() => recentlyOpened(placed, cards), [placed, cards]);

  return (
    <div className={styles.page}>
      <AppTopBar nav={{ label: 'Workspaces', to: '/workspace' }}>
        <span>
          <strong>{counts.working}</strong> working
        </span>
        <span data-attention={counts.waiting > 0 || undefined}>
          <strong>{counts.waiting}</strong> {counts.waiting === 1 ? 'needs' : 'need'} you
        </span>
      </AppTopBar>
      <main className={styles.gridArea} aria-label="Fleet">
        {(loadError || newWindowError) && <p className={styles.notice} role="alert">{newWindowError || loadError}</p>}
        {onBoard.length > 0 ? (
          <FleetGrid
            order={onBoard}
            cards={cards}
            selectedId={selectedId}
            live={live}
            faces={faces}
            now={clock}
            onSelect={select}
            onOpen={openWorkspace}
            onOpenWindow={openNewWindow}
            onDismiss={dismiss}
          />
        ) : (
          loadedAt != null &&
          (placed.length > 0 ? (
            <div className={styles.empty}>
              <h2>Every card is dismissed</h2>
              <p>A card comes back when something happens in its workspace; Ctrl+Tab brings one back now.</p>
            </div>
          ) : (
            <div className={styles.empty}>
              <h2>Nothing active in the last {RECENT_HOURS} hours</h2>
              <p>Type below to start a new workspace, or open the workspaces page.</p>
            </div>
          ))
        )}
      </main>
      <FleetSwitcher
        order={switcherOrder}
        cards={cards}
        dismissed={offBoard}
        selectedId={selectedId}
        live={live}
        faces={faces}
        onSelect={select}
      />
    </div>
  );
};

export default FleetPage;
