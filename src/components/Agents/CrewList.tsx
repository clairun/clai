/**
 * The workspace's crew, as the Agents drawer shows it: the Main pinned first,
 * then every agent added from the library. Each row leads with the agent's
 * face, whose ring says what it is doing right now — derived from the task
 * list, not from any stored state. "+ Add" in the drawer header unfolds the
 * card picker inside the drawer so nobody has to round-trip through the
 * settings modal; the parent owns that flag because the drawer header — the
 * parent's own markup — carries the toggle that flips it.
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { listAgentDefinitions, type AgentDefinitionDetail } from '../../api/client';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';
import { openGlobalSettings } from '../../utils/globalSettings';
import { isTaskActive } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import AgentCardPicker from './AgentCardPicker';
import { agentActivity, identityFor, type AgentActivity } from './agentIdentity';
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
  /** Whether the picker is unfolded. One owner: the parent, which renders the toggle. */
  pickerOpen: boolean;
  onOpenPicker: () => void;
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

export interface CrewStatus {
  /** What the ring draws. Disabled beats everything, then running, then attention. */
  activity: AgentActivity;
  /** One line about it, or `null` when there is nothing to say. */
  line: string | null;
}

/**
 * What a row shows about an agent. The ring follows `agentActivity` — the
 * same rule the header facepile draws — so the two can never disagree; this
 * only adds the wording.
 */
export const crewStatus = (
  agent: Pick<WorkspaceAgentResponse, 'id' | 'enabled'>,
  tasks: readonly WorkspaceTaskResponse[]
): CrewStatus => {
  const activity = agentActivity(agent, tasks);
  if (activity === 'disabled') return { activity, line: 'Disabled in this workspace' };
  if (activity === 'running') {
    const running = tasks.filter(
      (task) => task.assignedToWorkspaceAgentId === agent.id && isTaskActive(task)
    ).length;
    return { activity, line: running === 1 ? '1 task running' : `${running} tasks running` };
  }
  return { activity, line: activity === 'attention' ? 'Needs your review' : null };
};

const CrewList = ({
  workspaceId,
  agents,
  tasks,
  manageable,
  busy = '',
  error = '',
  pickerOpen,
  onOpenPicker,
  onOpenEdit,
  onRemove,
  onChanged,
}: CrewListProps) => {
  const crew = useMemo(() => sortCrew(agents), [agents]);
  const showPicker = manageable && pickerOpen;
  // Nobody to delegate to yet: no crew at all, or the Main by itself.
  const inviteToAdd = manageable && !pickerOpen && crew.every((agent) => agent.isDefault);

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
            const { activity, line } = crewStatus(agent, tasks);
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
                    {!agent.isDefault && <span className={styles.role}>shared</span>}
                  </div>
                  {description && <p className={styles.description}>{description}</p>}
                  {line && (
                    <span className={styles.live} data-activity={activity}>
                      {line}
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

      {inviteToAdd && (
        <div className={styles.invitation}>
          <p className={styles.invitationText}>
            {crew.length === 0
              ? 'No agents here yet. Add one from the library, or create one.'
              : 'Main works alone here. Add an agent from the library, or create one.'}
          </p>
          <button type="button" className={styles.action} onClick={onOpenPicker} disabled={!!busy}>
            Add to crew
          </button>
        </div>
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
              // The Main is not a library agent; the rows above already say who is on the crew.
              assignedDefinitionIds={crew
                .filter((agent) => !agent.isDefault)
                .map((agent) => agent.agentDefinitionId)}
              onChanged={handleCrewChanged}
              onCreateAgent={() => openGlobalSettings({ tab: 'agents' })}
              showCrewFaces={false}
              // The drawer is narrow and keeps its width: one agent per row.
              layout="rows"
              disabled={!!busy}
            />
          )}
        </div>
      )}
    </section>
  );
};

export default CrewList;
