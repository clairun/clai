/**
 * The agent library as a gallery of cards: agents any workspace can put on
 * its crew. Click a card to open its editor; the primary button starts a
 * new one. Archived agents sort last and desaturate; a search box narrows
 * the grid by name or description.
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import type { AgentDefinitionDetail } from '../../api/client';
import AgentAvatar from './AgentAvatar';
import { cardFacts } from './AgentCardPicker';
import { identityFor } from './agentIdentity';
import styles from './AgentGallery.module.css';

export interface AgentGalleryProps {
  definitions: readonly AgentDefinitionDetail[];
  onOpen: (id: string) => void;
  onCreate: () => void;
  /** Card to focus once, e.g. the agent just created. */
  focusId?: string | null;
  disabled?: boolean;
}

/** Live agents by name, archived after them, only those matching the query. */
export const galleryOrder = (
  definitions: readonly AgentDefinitionDetail[],
  query = ''
): AgentDefinitionDetail[] => {
  const needle = query.trim().toLowerCase();
  return definitions
    .filter(
      (definition) =>
        !needle ||
        definition.name.toLowerCase().includes(needle) ||
        definition.description.toLowerCase().includes(needle)
    )
    .sort((a, b) => {
      if (a.archived !== b.archived) return a.archived ? 1 : -1;
      return a.name.localeCompare(b.name);
    });
};

export const usageSentence = (
  definition: Pick<AgentDefinitionDetail, 'assignedWorkspaces' | 'archived'>
) => {
  const count = definition.assignedWorkspaces.length;
  if (definition.archived)
    return count === 0
      ? 'Archived'
      : `Archived, still on ${count} workspace${count === 1 ? '' : 's'}`;
  if (count === 0) return 'Not on any workspace';
  return `On ${count} workspace${count === 1 ? '' : 's'}`;
};

const AgentGallery = ({
  definitions,
  onOpen,
  onCreate,
  focusId = null,
  disabled = false,
}: AgentGalleryProps) => {
  const [query, setQuery] = useState('');
  const ordered = useMemo(() => galleryOrder(definitions, query), [definitions, query]);
  const focusRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (focusId) focusRef.current?.focus();
  }, [focusId]);

  return (
    <div className={styles.gallery}>
      <div className={styles.header}>
        <div className={styles.headerText}>
          <h3 className={styles.title}>Agents</h3>
          <p className={styles.description}>
            Agents you can put on any workspace's crew. Edits reach every workspace on its next
            turn.
          </p>
        </div>
        <button type="button" className={styles.create} onClick={onCreate} disabled={disabled}>
          + Create agent
        </button>
      </div>

      {definitions.length > 0 && (
        <input
          type="search"
          className={styles.search}
          placeholder="Search agents"
          aria-label="Search agents"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      )}

      {ordered.length > 0 ? (
        <div className={styles.grid}>
          {ordered.map((definition) => (
            <button
              key={definition.id}
              type="button"
              ref={definition.id === focusId ? focusRef : undefined}
              className={`${styles.card} ${definition.archived ? styles.cardArchived : ''}`}
              onClick={() => onOpen(definition.id)}
              disabled={disabled}
            >
              <AgentAvatar
                identity={identityFor({ id: definition.id, avatar: definition.avatar })}
                size={56}
                activity={definition.archived ? 'disabled' : 'none'}
              />
              <span className={styles.cardName}>{definition.name}</span>
              {definition.description && (
                <span className={styles.cardDescription}>{definition.description}</span>
              )}
              <span className={styles.cardFacts}>{cardFacts(definition)}</span>
              <span className={styles.cardUsage}>{usageSentence(definition)}</span>
            </button>
          ))}
        </div>
      ) : (
        <p className={styles.empty}>
          {definitions.length === 0
            ? 'No agents yet. Create the first one.'
            : 'No agent matches that search.'}
        </p>
      )}
    </div>
  );
};

export default AgentGallery;
