/**
 * The workspace's crew, as the Agents drawer shows it: the Main pinned first,
 * then every agent added from the library. Each row leads with the agent's
 * face, whose ring says what it is doing right now — derived from the task
 * list, not from any stored state. "+ Add" unfolds the card picker inside the
 * drawer so nobody has to round-trip through the settings modal.
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { listAgentDefinitions, type AgentDefinitionDetail } from '../../api/client';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';
import { openGlobalSettings } from '../../utils/globalSettings';
import AgentAvatar from './AgentAvatar';
import AgentCardPicker from './AgentCardPicker';
import { activityFromTasks, identityFor, type AgentActivity } from './agentIdentity';
import styles from './CrewList.module.css';

export interface CrewListProps {
  workspaceId: string;
  agents: readonly WorkspaceAgentResponse[];
  tasks: readonly WorkspaceTaskResponse[];
  /** False for agent-kind and default workspaces: rows only, no actions. */
  manageable: boolean;
  /** The parent's in-flight action id (e.g. `remove:<id>`), if any. */
  busy?: string;
  error?: string;
  /** Whether the picker is unfolded; the parent owns it because the drawer widens with it. */
  pickerOpen: boolean;
  onOpenEdit: (workspaceAgentId: string) => void;
  onRemove: (workspaceAgentId: string) => void;
  /** After the crew changed through the picker; the parent reloads its snapshot. */
  onChanged: () => void | Promise<void>;
}

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

/** Main first, then the rest in the order the snapshot gave them. */
export const sortCrew = (agents: readonly WorkspaceAgentResponse[]): WorkspaceAgentResponse[] =>
  [...agents].sort((a, b) => Number(b.isDefault) - Number(a.isDefault));

/** The row's activity: disabled beats everything, then what the tasks say. */
export const crewActivity = (
  agent: Pick<WorkspaceAgentResponse, 'id' | 'enabled'>,
  tasks: readonly WorkspaceTaskResponse[]
): AgentActivity => (agent.enabled ? activityFromTasks(agent.id, tasks) : 'disabled');

/** One line about what the agent is doing, or `null` when there is nothing to say. */
export const liveLine = (
  agentId: string,
  tasks: readonly WorkspaceTaskResponse[]
): string | null => {
  const mine = tasks.filter((task) => task.assignedToWorkspaceAgentId === agentId);
  const running = mine.filter(
    (task) => task.status === 'running' || task.status === 'queued'
  ).length;
  if (running > 0) return running === 1 ? '1 task running' : `${running} tasks running`;
  const attention = mine.some(
    (task) =>
      (task.status === 'blocked' || task.status === 'failed') &&
      !task.attentionAcknowledgedAt &&
      !task.userResponseAt
  );
  return attention ? 'Needs your review' : null;
};

const CrewList = ({
  workspaceId,
  agents,
  tasks,
  manageable,
  busy = '',
  error = '',
  pickerOpen,
  onOpenEdit,
  onRemove,
  onChanged,
}: CrewListProps) => {
  const crew = useMemo(() => sortCrew(agents), [agents]);
  // The Main working alone is the one time the picker opens by itself: the
  // drawer would otherwise be one row and a lot of nothing.
  const mainAlone = manageable && crew.every((agent) => agent.isDefault);
  const showPicker = manageable && (pickerOpen || mainAlone);

  const [library, setLibrary] = useState<AgentDefinitionDetail[] | null>(null);
  const [libraryError, setLibraryError] = useState<string | null>(null);
  const loadLibrary = useCallback(
    () =>
      listAgentDefinitions()
        .then((definitions) => {
          setLibrary(definitions);
          setLibraryError(null);
        })
        .catch((err: unknown) => {
          setLibraryError(errText(err, 'Failed to load the agent library.'));
        }),
    []
  );

  // The library is global, so a late response is never stale for another
  // workspace; no cancellation needed.
  useEffect(() => {
    if (showPicker) void loadLibrary();
  }, [showPicker, loadLibrary]);

  const handleCrewChanged = useCallback(async () => {
    await onChanged();
    // A definition assigned elsewhere meanwhile, or created in Settings, only
    // shows up if the library is re-read too.
    await loadLibrary();
  }, [onChanged, loadLibrary]);

  if (!manageable && crew.length === 0) return null;

  return (
    <section className={styles.crew} aria-label="Workspace crew">
      {error && <div className={styles.error}>{error}</div>}

      {crew.length > 0 && (
        <ul className={styles.list}>
          {crew.map((agent) => {
            const activity = crewActivity(agent, tasks);
            const live = agent.enabled ? liveLine(agent.id, tasks) : 'Disabled in this workspace';
            const name = agent.isDefault
              ? 'Main'
              : agent.displayName || agent.agentName || 'Untitled';
            const description = agent.isDefault
              ? "This workspace's own agent."
              : agent.agentDescription;
            return (
              <li key={agent.id} className={styles.row} data-activity={activity}>
                <AgentAvatar identity={identityFor(agent)} size={36} activity={activity} />
                <div className={styles.identity}>
                  <div className={styles.nameRow}>
                    <span className={styles.name}>{name}</span>
                    {!agent.isDefault && <span className={styles.role}>crew</span>}
                  </div>
                  {description && <p className={styles.description}>{description}</p>}
                  {live && (
                    <span className={styles.live} data-activity={activity}>
                      {live}
                    </span>
                  )}
                </div>
                {manageable && (
                  <div className={styles.actions}>
                    <button
                      type="button"
                      className={styles.action}
                      onClick={() => onOpenEdit(agent.id)}
                      disabled={!!busy}
                    >
                      Edit
                    </button>
                    {!agent.isDefault && (
                      <button
                        type="button"
                        className={`${styles.action} ${styles.actionDanger}`}
                        onClick={() => onRemove(agent.id)}
                        disabled={!!busy}
                        aria-label={`Remove ${name} from the crew`}
                      >
                        Remove
                      </button>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {showPicker && (
        <div className={styles.picker} data-testid="crew-picker">
          <h3 className={styles.pickerTitle}>Add to crew</h3>
          {libraryError && <div className={styles.error}>{libraryError}</div>}
          {library === null && !libraryError && (
            <p className={styles.loading}>Loading the library…</p>
          )}
          {library !== null && (
            <AgentCardPicker
              workspaceId={workspaceId}
              definitions={library}
              assignedDefinitionIds={crew.map((agent) => agent.agentDefinitionId)}
              onChanged={handleCrewChanged}
              onCreateAgent={() => openGlobalSettings({ tab: 'agents' })}
              disabled={!!busy}
              intro={
                mainAlone
                  ? 'Main works alone here. Add crew from the library, or create one.'
                  : 'Pick from the library. Behaviour stays shared; this workspace decides context and paths.'
              }
            />
          )}
        </div>
      )}
    </section>
  );
};

export default CrewList;
