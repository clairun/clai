import { useEffect, useMemo, useRef, useState } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import { useFleetStore } from '../../fleet/fleetStore';
import { listWorkspaces } from '../../workspace/client';
import type { WorkspaceListEntry } from '../../generated/bindings';
import { formatRelativeTime } from '../../utils/taskDisplay';
import styles from './TerminalEmulator.module.css';

interface PickerEntry {
  id: string;
  title: string;
  detail: string;
  /** Its Main's face; only the Fleet's workspaces know their Main. */
  main: AgentIdentity;
}

/** The create entry: no selection, so the next prompt creates a workspace. */
const CREATE = null;

export const PICKER_SHORTCUT = 'Ctrl+K';

/**
 * Searchable list of every workspace, for the Fleet terminal's target:
 * the Fleet's first, then the rest by recency. Picking one selects it,
 * which puts it on the stage however old it is. Keyboard first: the search
 * has focus, the first match is highlighted, Enter picks, Esc closes.
 */
const WorkspacePicker = ({ onClose }: { onClose: () => void }) => {
  const cards = useFleetStore((s) => s.fleet.cards);
  const [all, setAll] = useState<WorkspaceListEntry[]>([]);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState<number | null>(null);
  const listRef = useRef<HTMLUListElement | null>(null);

  useEffect(() => {
    let cancelled = false;
    listWorkspaces()
      .then((list) => !cancelled && setAll(list))
      .catch(() => {
        // The Fleet's own workspaces still show.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const entries = useMemo((): PickerEntry[] => {
    const onFleet = Object.values(cards)
      .sort((a, b) => b.lastActivityAt - a.lastActivityAt)
      .map((card) => ({
        id: card.workspaceId,
        title: card.title || 'Workspace',
        detail: `On the Fleet · ${formatRelativeTime(card.lastActivityAt)}`,
        main: mainIdentity(card.main),
      }));
    const rest = all
      .filter((w) => !cards[w.id])
      .sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt))
      .map((w) => ({
        id: w.id,
        title: w.title || 'Workspace',
        detail: `${Number(w.messageCount)} messages · ${formatRelativeTime(w.updatedAt)}`,
        main: mainIdentity(),
      }));
    return [...onFleet, ...rest];
  }, [cards, all]);

  const needle = query.trim().toLowerCase();
  const matches = needle ? entries.filter((e) => e.title.toLowerCase().includes(needle)) : entries;
  const options: (PickerEntry | typeof CREATE)[] = [CREATE, ...matches];
  const highlighted = active ?? (needle && matches.length > 0 ? 1 : 0);

  useEffect(() => {
    listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' });
  }, [highlighted]);

  const pick = (entry: PickerEntry | typeof CREATE) => {
    const { fleet, select, loadSnapshot } = useFleetStore.getState();
    select(entry?.id ?? null);
    // Not among the loaded cards: the snapshot keeps the selected one.
    if (entry && !fleet.cards[entry.id]) void loadSnapshot();
    onClose();
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      setActive((highlighted + step + options.length) % options.length);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(options[highlighted] ?? CREATE);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  };

  const optionId = (index: number) => `workspace-picker-option-${index}`;
  return (
    <div className={styles.picker} role="dialog" aria-label="Choose the target workspace">
      <input
        autoFocus
        className={styles.pickerSearch}
        role="combobox"
        aria-expanded="true"
        aria-controls="workspace-picker-list"
        aria-activedescendant={optionId(highlighted)}
        aria-label="Search workspaces"
        placeholder={`Search workspaces (${PICKER_SHORTCUT})`}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(null);
        }}
        onKeyDown={onKeyDown}
        onBlur={onClose}
        spellCheck={false}
        autoComplete="off"
      />
      <ul ref={listRef} id="workspace-picker-list" className={styles.pickerList} role="listbox">
        {options.map((entry, index) => (
          <li
            key={entry?.id ?? 'create'}
            id={optionId(index)}
            role="option"
            aria-selected={index === highlighted}
            className={entry ? styles.pickerOption : `${styles.pickerOption} ${styles.pickerCreate}`}
            // Keep focus in the search, so a click doesn't close the picker first.
            onMouseDown={(e) => e.preventDefault()}
            onMouseEnter={() => setActive(index)}
            onClick={() => pick(entry)}
          >
            {entry ? (
              <>
                <AgentAvatar identity={entry.main} size={20} ring={false} label="Main" />
                <span className={styles.pickerTitle}>{entry.title}</span>
                <span className={styles.pickerDetail}>{entry.detail}</span>
              </>
            ) : (
              <>
                <span className={styles.createIcon} aria-hidden="true">+</span>
                <span className={styles.pickerTitle}>Create workspace</span>
              </>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
};

export default WorkspacePicker;
