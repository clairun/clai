/**
 * "Add to crew": the library as cards, one click per agent. Assigning is
 * local to the workspace and reversible, so there is no confirm step — the
 * card leaves the row and an Undo appears in its place for a moment.
 *
 * The parent hands in what it already knows (the library and who is on the
 * crew) and reloads after `onChanged`; this component only owns the two calls
 * that change the crew and the transient state around them.
 */
import { useCallback, useState } from 'react';
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
  /** Shown as the escape hatch when the library has nothing left, or always as a link. */
  onCreateAgent?: () => void;
  disabled?: boolean;
  /** One sentence for the empty roster; default copy explains the split. */
  intro?: string;
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

interface Added {
  workspaceAgentId: string;
  name: string;
}

const AgentCardPicker = ({
  workspaceId,
  definitions,
  assignedDefinitionIds,
  onChanged,
  onCreateAgent,
  disabled = false,
  intro = 'Pick from the library. Behaviour stays shared; this workspace decides context and paths.',
}: AgentCardPickerProps) => {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<Added | null>(null);

  const available = availableDefinitions(definitions, assignedDefinitionIds);
  const onCrew = definitions.filter((item) => assignedDefinitionIds.includes(item.id));

  const assign = useCallback(
    async (definition: AgentDefinitionDetail) => {
      if (busy) return;
      setBusy(true);
      setError(null);
      try {
        const workspaceAgentId = await assignWorkspaceAgent(workspaceId, definition.id);
        setAdded({ workspaceAgentId, name: definition.name });
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
        <div className={styles.cards} role="list">
          {available.map((definition) => (
            <button
              key={definition.id}
              type="button"
              role="listitem"
              className={styles.card}
              onClick={() => assign(definition)}
              disabled={inert}
              aria-label={`Add ${definition.name} to the crew`}
            >
              <AgentAvatar
                identity={identityFor({ id: definition.id, avatar: definition.avatar })}
                size={48}
              />
              <span className={styles.cardName}>{definition.name}</span>
              {definition.description && (
                <span className={styles.cardDescription}>{definition.description}</span>
              )}
              <span className={styles.cardFacts}>{cardFacts(definition)}</span>
            </button>
          ))}
        </div>
      ) : (
        <p className={styles.empty}>
          {definitions.length === 0
            ? 'The library is empty.'
            : 'Every agent in the library is already on this crew.'}
        </p>
      )}

      <div className={styles.footer}>
        {onCrew.length > 0 && (
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
