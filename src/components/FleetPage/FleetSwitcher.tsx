import { useEffect, useMemo, useState } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { faceOf, type AgentIdentity } from '../Agents/agentIdentity';
import {
  cardAttention,
  cardAgents,
  cardPhase,
  type FleetCard,
  type FleetLive,
} from '../../fleet/fleetModel';
import { dispatchFocusTerminal } from '../../utils/workspaceUiEvents';
import { phaseLabel } from './FleetCard';
import styles from './FleetPage.module.css';

interface FleetSwitcherProps {
  /** Card ids, most recently opened first (`recentlyOpened`), each one in `cards`. */
  order: string[];
  cards: Record<string, FleetCard>;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  /** `null`: the "New workspace" entry, nothing selected. */
  onSelect: (workspaceId: string | null) => void;
}

/** The last entry: deselects, so the terminal starts a new workspace. */
const NEW_WORKSPACE = 'fleet:new-workspace';

/** The entry `step` away from `current`, wrapping; from an unknown one, the first or the last. */
const cycleCard = (entries: readonly string[], current: string, step: 1 | -1): string => {
  const at = entries.indexOf(current);
  if (at < 0) return step > 0 ? entries[0]! : entries[entries.length - 1]!;
  return entries[(at + step + entries.length) % entries.length]!;
};

/**
 * The OS-style switcher, from anywhere on the page, the terminal included
 * (capture beats the terminal's keydown). Ctrl+Tab opens it; while Ctrl is
 * held, Tab / Shift+Tab move only its highlight. Releasing Ctrl selects the
 * highlighted card once, as Enter does ("New workspace", the last entry,
 * deselects), and puts the keyboard in the terminal; Escape or leaving the
 * window closes it unchanged.
 * Static: no animation, no blur.
 */
const FleetSwitcher = ({ order, cards, selectedId, live, faces, onSelect }: FleetSwitcherProps) => {
  const [highlightId, setHighlightId] = useState<string | null>(null);
  const entries = useMemo(() => [...order, NEW_WORKSPACE], [order]);

  useEffect(() => {
    // The choice, then the keyboard in the terminal, which talks to it.
    const choose = (id: string) => {
      onSelect(id === NEW_WORKSPACE ? null : id);
      setHighlightId(null);
      dispatchFocusTerminal();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (highlightId && e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        setHighlightId(null);
        return;
      }
      if (highlightId && e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        choose(highlightId);
        return;
      }
      if (e.code !== 'Tab' || !e.ctrlKey || e.altKey || e.metaKey || order.length === 0) return;
      e.preventDefault();
      e.stopPropagation();
      setHighlightId(cycleCard(entries, highlightId ?? selectedId ?? NEW_WORKSPACE, e.shiftKey ? -1 : 1));
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (highlightId && e.key === 'Control') choose(highlightId);
    };
    const onBlur = () => setHighlightId(null);
    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('blur', onBlur);
    };
  }, [order, entries, selectedId, highlightId, onSelect]);

  if (!highlightId) return null;
  return (
    <div className={styles.switcherBackdrop}>
      <ul className={styles.switcher} role="listbox" aria-label="Switch workspace">
        {order.map((id) => {
          const card = cards[id]!;
          const agents = cardAgents(card, live);
          const phase = cardPhase(card, live);
          return (
            <li
              key={id}
              className={styles.switcherRow}
              role="option"
              aria-selected={id === highlightId}
            >
              <span className={styles.switcherTitle}>{card.title || 'Workspace'}</span>
              <span className={styles.faces}>
                {agents.slice(0, 4).map((agent) => (
                  <AgentAvatar
                    key={agent.seed}
                    identity={faceOf(faces, agent.seed)}
                    size={20}
                    ring={false}
                    label={agent.name}
                  />
                ))}
              </span>
              <span className={styles.phase} data-phase={phase}>
                {phaseLabel(phase, cardAttention(card))}
              </span>
            </li>
          );
        })}
        <li className={styles.switcherRow} role="option" aria-selected={highlightId === NEW_WORKSPACE}>
          <span className={styles.switcherTitle}>New workspace</span>
        </li>
      </ul>
    </div>
  );
};

export default FleetSwitcher;
