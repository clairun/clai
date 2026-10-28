import { useCallback, useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router';
import AppTopBar from '../components/AppTopBar';
import FleetGrid from '../components/FleetPage/FleetGrid';
import FleetSwitcher from '../components/FleetPage/FleetSwitcher';
import styles from '../components/FleetPage/FleetPage.module.css';
import { listAgentDefinitions } from '../api/client';
import { identityFor, type AgentIdentity } from '../components/Agents/agentIdentity';
import {
  RECENT_WINDOW_MS,
  cardPhase,
  mainRunning,
  eligibleCards,
  recentlyOpened,
  type FleetLive,
} from '../fleet/fleetModel';
import { useFleetStore } from '../fleet/fleetStore';
import { useMinute } from '../fleet/useMinute';
import { useFleetToggle } from '../fleet/useFleetToggle';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import { markWorkspaceOpened } from '../workspace/client';

const RECENT_HOURS = RECENT_WINDOW_MS / 3_600_000;

/**
 * Where Enter does its own thing, so it doesn't open the selected workspace.
 * A card's title is a button only to select the card, so Enter there opens.
 */
const OWNS_ENTER = 'input, textarea, select, a, button:not([data-card-title]), [contenteditable="true"], [role="dialog"]';
/** A zoom or a modal; an agent question sits inline in its card (see Escape above). */
const OPEN_DIALOG = '[role="dialog"]:not([aria-label="Agent question"])';

/**
 * The Fleet: one card per recently active workspace, with the shared
 * terminal at the bottom. Clicking a card selects it: it is highlighted and
 * the terminal talks to it; with nothing selected the terminal starts a new
 * workspace.
 */
const FleetPage = () => {
  const cards = useFleetStore((s) => s.fleet.cards);
  const now = useFleetStore((s) => s.now);
  // Panels are judged against this: freshness, shouts and captions age on the
  // minute tick, while what a panel says changes with events.
  const clock = Math.max(now, useMinute());
  const selectedId = useFleetStore((s) => s.selectedId);
  const select = useFleetStore((s) => s.select);
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

  const placed = useMemo(
    () => eligibleCards(Object.values(cards), { now, selectedId, live }),
    [cards, now, selectedId, live]
  );

  // Escape deselects, unless something else used it first (the switcher,
  // the terminal clearing its output, or an open dialog). Agent questions
  // sit inline in the cards; they don't hold Escape.
  useEffect(() => {
    const onEscape = (e: KeyboardEvent) => {
      const dialog = document.querySelector('[role="dialog"]:not([aria-label="Agent question"])');
      if (e.key === 'Escape' && !e.defaultPrevented && !dialog) select(null);
    };
    window.addEventListener('keydown', onEscape);
    return () => window.removeEventListener('keydown', onEscape);
  }, [select]);

  // Selected is seen: the rail's unread dot clears, as on the Workspace page
  // (see the same stamp there), and again on leaving while Main is working.
  const selectedWorking = !!selectedId && !!cards[selectedId] && mainRunning(cards[selectedId], live);
  useEffect(() => {
    if (!selectedId) return undefined;
    markWorkspaceOpened(selectedId).catch(() => {});
    return () => {
      markWorkspaceOpened(selectedId).catch(() => {});
    };
  }, [selectedId, selectedWorking]);

  // Cards born from live events (or a workspace just created here) have no
  // title yet: the snapshot has it, once per new set of untitled cards.
  const untitled = placed.filter((id) => !cards[id]?.title).join(',');
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

  // Opening a workspace: the card's icon, Enter, or Ctrl+Shift+F with a card selected.
  const navigate = useNavigate();
  const openWorkspace = useCallback((id: string) => navigate(`/workspace/${id}`), [navigate]);
  useFleetToggle(useMemo(() => (selectedId ? () => openWorkspace(selectedId) : null), [selectedId, openWorkspace]));
  useEffect(() => {
    if (!selectedId) return undefined;
    const onEnter = (e: KeyboardEvent) => {
      if (e.key !== 'Enter' || e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return;
      if ((e.target instanceof Element && e.target.closest(OWNS_ENTER)) || document.querySelector(OPEN_DIALOG)) return;
      e.preventDefault();
      openWorkspace(selectedId);
    };
    window.addEventListener('keydown', onEnter);
    return () => window.removeEventListener('keydown', onEnter);
  }, [selectedId, openWorkspace]);

  const switcherOrder = useMemo(() => recentlyOpened(placed, cards), [placed, cards]);
  const handleSelect = useCallback((id: string | null) => select(id), [select]);

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
        {loadError && <p className={styles.notice}>{loadError}</p>}
        {placed.length > 0 ? (
          <FleetGrid
            order={placed}
            cards={cards}
            selectedId={selectedId}
            live={live}
            faces={faces}
            now={clock}
            onSelect={handleSelect}
            onOpen={openWorkspace}
          />
        ) : (
          loadedAt != null && (
            <div className={styles.empty}>
              <h2>Nothing active in the last {RECENT_HOURS} hours</h2>
              <p>Type below to start a new workspace, or open the workspaces page.</p>
            </div>
          )
        )}
      </main>
      <FleetSwitcher order={switcherOrder} cards={cards} selectedId={selectedId} live={live} faces={faces} onSelect={handleSelect} />
    </div>
  );
};

export default FleetPage;
