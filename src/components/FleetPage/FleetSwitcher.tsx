import { useEffect, useState } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { GENERATOR_VERSION, type AgentIdentity } from '../Agents/agentIdentity';
import {
  cardAttention,
  cardItems,
  cardPhase,
  cycleCard,
  type FleetCard,
  type FleetLive,
} from '../../fleet/fleetModel';
import { phaseLabel } from './FleetCard';
import styles from './FleetPage.module.css';

interface FleetSwitcherProps {
  /** Card ids in slot order. */
  order: string[];
  cards: Record<string, FleetCard>;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  onSelect: (workspaceId: string) => void;
}

/**
 * The OS-style switcher, from anywhere on the page, the terminal included
 * (capture beats the terminal's keydown). Ctrl+Tab opens it; while Ctrl is
 * held, Tab / Shift+Tab move only its highlight. Releasing Ctrl selects the
 * highlighted card once; Escape or leaving the window closes it unchanged.
 * Static: no animation, no blur.
 */
const FleetSwitcher = ({ order, cards, selectedId, live, faces, onSelect }: FleetSwitcherProps) => {
  const [highlightId, setHighlightId] = useState<string | null>(null);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (highlightId && e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        setHighlightId(null);
        return;
      }
      if (e.key !== 'Tab' || !e.ctrlKey || e.altKey || e.metaKey || order.length === 0) return;
      e.preventDefault();
      e.stopPropagation();
      setHighlightId(cycleCard(order, highlightId ?? selectedId, e.shiftKey ? -1 : 1));
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (highlightId && e.key === 'Control') {
        onSelect(highlightId);
        setHighlightId(null);
      }
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
  }, [order, selectedId, highlightId, onSelect]);

  if (!highlightId) return null;
  return (
    <div className={styles.switcherBackdrop}>
      <ul className={styles.switcher} role="listbox" aria-label="Switch workspace">
        {order.map((id) => {
          const card = cards[id];
          if (!card) return null;
          const agents = [
            ...new Map(
              cardItems(card, live)
                .flatMap((i) => i.agents)
                .map((a) => [a.seed, a])
            ).values(),
          ];
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
                    identity={
                      faces[agent.seed] ?? { seed: agent.seed, generatorVersion: GENERATOR_VERSION }
                    }
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
      </ul>
    </div>
  );
};

export default FleetSwitcher;
