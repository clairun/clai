/**
 * "Add to crew": the library as cards, one click per agent. Assigning is
 * local to the workspace and reversible, so there is no confirm step — the
 * card leaves the row and an Undo appears in its place until the crew moves
 * on.
 *
 * The parent hands in what it already knows (the library and who is on the
 * crew) and reloads after `onChanged`; this component only owns the two calls
 * that change the crew and the transient state around them.
 */
import { useCallback, useEffect, useState } from 'react';
import {
  assignWorkspaceAgent,
  workspaceDeleteAgent,
  type AgentDefinitionDetail,
} from '../../api/client';
import AgentAvatar from './AgentAvatar';
import { identityFor } from './agentIdentity';
import styles from './AgentCardPicker.module.css';

export interface AgentCardPickerProps {
  workspaceId: string;
  definitions: AgentDefinitionDetail[];
  /** Definition ids already on this workspace; their cards are not offered again. */
  assignedDefinitionIds: readonly string[];
  /** After an assign or an undo went through; the parent refreshes its roster. */
  onChanged?: () => void | Promise<void>;
  /** Offered as a link when given; otherwise the copy points at Settings → Agents. */
  onCreateAgent?: () => void;
  disabled?: boolean;
  /** The sentence above the cards. */
  intro?: string;
  /** The "Already on this crew" face row; off where the crew is already on screen. */
  showCrewFaces?: boolean;
  /**
   * How the cards are laid out. `grid` (default) tiles them; `rows` lays each
   * card down on its own line, for narrow hosts like the Agents drawer.
   */
  layout?: 'grid' | 'rows';
}

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? '' : 's'}`;

/** Two facts that tell agents with similar names apart. */
export const cardFacts = (definition: AgentDefinitionDetail): string => {
  const facts: string[] = [];
  if (definition.selectedSkillIds.length > 0)
    facts.push(plural(definition.selectedSkillIds.length, 'skill'));
  if (definition.selectedMcpServerIds.length > 0) {
    facts.push(plural(definition.selectedMcpServerIds.length, 'MCP server'));
  }
  return facts.length > 0 ? facts.join(', ') : 'No skills or MCP servers';
};

/** What the picker offers: live definitions not yet on the crew. */
export const availableDefinitions = (
  definitions: readonly AgentDefinitionDetail[],
  assignedDefinitionIds: readonly string[]
): AgentDefinitionDetail[] =>
  definitions.filter((item) => !item.archived && !assignedDefinitionIds.includes(item.id));

/** Why there is nothing to pick, with the way out when no link is offered. */
export const emptyCopy = (libraryEmpty: boolean, hasCreateLink: boolean): string => {
  const reason = libraryEmpty
    ? 'The library is empty.'
    : 'Every agent in the library is already on this crew.';
  return hasCreateLink ? reason : `${reason} Create one in Settings → Agents.`;
};

interface Added {
  workspaceAgentId: string;
  definitionId: string;
  name: string;
  /** Set once the parent's roster has shown the agent; after that, its leaving ends the Undo. */
  seenOnCrew: boolean;
}

const AgentCardPicker = ({
  workspaceId,
  definitions,
  assignedDefinitionIds,
  onChanged,
  onCreateAgent,
  disabled = false,
  intro = 'Pick from the library. Behavior stays shared; this workspace decides context and paths.',
  showCrewFaces = true,
  layout = 'grid',
}: AgentCardPickerProps) => {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<Added | null>(null);

  const available = availableDefinitions(definitions, assignedDefinitionIds);
  const onCrew = definitions.filter((item) => assignedDefinitionIds.includes(item.id));

  // The Undo is for the assign just made. Once the roster has shown the agent
  // and it leaves again by any other route (the row's Remove, another
  // window), the offer is stale and goes away instead of failing later.
  const addedOnCrew = added !== null && assignedDefinitionIds.includes(added.definitionId);
  useEffect(() => {
    if (!added) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reconciling the Undo offer with the roster the parent hands down; fires only on the two transitions described above.
    if (addedOnCrew && !added.seenOnCrew) setAdded({ ...added, seenOnCrew: true });
    else if (!addedOnCrew && added.seenOnCrew) setAdded(null);
  }, [added, addedOnCrew]);

  const assign = useCallback(
    async (definition: AgentDefinitionDetail) => {
      if (busy) return;
      setBusy(true);
      setError(null);
      try {
        const workspaceAgentId = await assignWorkspaceAgent(workspaceId, definition.id);
        setAdded({
          workspaceAgentId,
          definitionId: definition.id,
          name: definition.name,
          seenOnCrew: false,
        });
        // Held busy until the parent has reloaded: a second assign against the
        // old roster would be refused by the backend as a duplicate.
        await onChanged?.();
      } catch (err) {
        setError(errText(err, 'Failed to add the agent.'));
      } finally {
        setBusy(false);
      }
    },
    [busy, workspaceId, onChanged]
  );

  const undo = useCallback(async () => {
    if (busy || !added) return;
    setBusy(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, added.workspaceAgentId);
      setAdded(null);
      await onChanged?.();
    } catch (err) {
      setError(errText(err, 'Failed to undo.'));
    } finally {
      setBusy(false);
    }
  }, [busy, added, workspaceId, onChanged]);

  const inert = disabled || busy;
  const asRows = layout === 'rows';

  return (
    <div className={styles.picker}>
      <p className={styles.intro}>{intro}</p>

      {error && (
        <div className={styles.error} role="alert">
          {error}
        </div>
      )}

      {added && (
        <div className={styles.added} role="status">
          <span>
            <strong>{added.name}</strong> joined the crew.
          </span>
          <button type="button" className={styles.undo} onClick={undo} disabled={inert}>
            Undo
          </button>
        </div>
      )}

      {available.length > 0 ? (
        <div className={`${styles.cards} ${asRows ? styles.cardsRows : ''}`} role="list">
          {available.map((definition) => (
            <button
              key={definition.id}
              type="button"
              role="listitem"
              className={`${styles.card} ${asRows ? styles.cardRow : ''}`}
              onClick={() => assign(definition)}
              disabled={inert}
              aria-label={`Add ${definition.name} to the crew`}
            >
              <AgentAvatar
                identity={identityFor({ id: definition.id, avatar: definition.avatar })}
                // In rows the face sits beside the text, at the size the crew
                // rows above the picker already use.
                size={asRows ? 36 : 48}
              />
              <span className={styles.cardBody}>
                <span className={styles.cardName}>{definition.name}</span>
                {definition.description && (
                  <span className={styles.cardDescription}>{definition.description}</span>
                )}
                <span className={styles.cardFacts}>{cardFacts(definition)}</span>
              </span>
            </button>
          ))}
        </div>
      ) : (
        <p className={styles.empty}>{emptyCopy(definitions.length === 0, !!onCreateAgent)}</p>
      )}

      <div className={styles.footer}>
        {showCrewFaces && onCrew.length > 0 && (
          <span className={styles.onCrew}>
            <span className={styles.onCrewLabel}>Already on this crew</span>
            <span className={styles.onCrewFaces}>
              {onCrew.map((definition) => (
                <AgentAvatar
                  key={definition.id}
                  identity={identityFor({ id: definition.id, avatar: definition.avatar })}
                  size={20}
                  label={definition.name}
                />
              ))}
            </span>
          </span>
        )}
        {onCreateAgent && (
          <button
            type="button"
            className={styles.createLink}
            onClick={onCreateAgent}
            disabled={inert}
          >
            Create a new agent →
          </button>
        )}
      </div>
    </div>
  );
};

export default AgentCardPicker;
