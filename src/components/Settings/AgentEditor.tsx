/**
 * One shared agent, open for editing — or a blank one being created. Replaces
 * the gallery in the same pane: breadcrumb back to Agents, the face row, the
 * behaviour form, then Save and Archive. The face choice lives here so it
 * starts over with every agent opened and can never be saved onto another.
 */
import { useCallback, useMemo, useRef, useState } from 'react';
import { saveAgentDefinition, type AgentDefinitionDetail } from '../../api/client';
import AgentFacePicker, {
  chosenSeed,
  freshFaceChoice,
  type FaceChoice,
} from '../Agents/AgentFacePicker';
import { avatarRefFor, identityFor } from '../Agents/agentIdentity';
import {
  AgentBehaviorForm,
  type AgentBehaviorPayload,
  type AgentDetail,
  type AgentFormDeps,
} from './AgentBehaviorForm';
import type { SectionHandle } from './sectionHandle';
import styles from './AgentEditor.module.css';

export interface AgentEditorProps {
  /** Omit to create a new agent. */
  definition?: AgentDefinitionDetail;
  deps: AgentFormDeps;
  onBack: () => void;
  /**
   * After a successful save: the saved id and whether it was just created.
   * The host reloads the library; a create returns to the gallery.
   */
  onSaved: (id: string, created: boolean) => void | Promise<void>;
}

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const AgentEditor = ({ definition, deps, onBack, onSaved }: AgentEditorProps) => {
  const [face, setFace] = useState<FaceChoice>(freshFaceChoice);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const sectionRef = useRef<SectionHandle | null>(null);

  const currentIdentity = useMemo(
    () => (definition ? identityFor({ id: definition.id, avatar: definition.avatar }) : null),
    [definition]
  );

  // The form speaks the workspace-agent shape; the editor adds identity, the
  // face and the revision the edit started from.
  const save = useCallback(
    async (payload: AgentBehaviorPayload) => {
      const seed = chosenSeed(face, currentIdentity);
      const id = await saveAgentDefinition({
        id: definition?.id,
        expectedRevision: definition?.revision,
        name: payload.name,
        description: payload.description,
        selectedSkillIds: payload.selectedSkillIds,
        selectedMcpServerIds: payload.selectedMcpServerIds,
        providerConnectionIds: payload.providerConnectionIds,
        execution: payload.execution,
        enabled: payload.enabled,
        archived: definition?.archived ?? false,
        // Omitting `avatar` keeps the stored face. A create always carries one.
        ...(seed === null ? {} : { avatar: avatarRefFor(seed) }),
      });
      // A saved pick is the stored face now: the row starts over on it. A save
      // that left the face alone leaves the row alone too.
      if (seed !== null) setFace(freshFaceChoice());
      await onSaved(id, !definition);
    },
    [definition, currentIdentity, face, onSaved]
  );

  const handleSave = useCallback(async () => {
    setSaving(true);
    setError(null);
    try {
      const result = await sectionRef.current?.submit();
      if (result && !result.ok) setError(result.error ?? 'Failed to save the agent.');
    } catch (err) {
      setError(errText(err, 'Failed to save the agent.'));
    } finally {
      setSaving(false);
    }
  }, []);

  // Archiving keeps history and in-flight runs intact but stops new
  // assignments and new runs — a shared definition can be referenced from
  // workspaces the user is not looking at, so deleting it outright would break
  // them silently.
  const handleArchiveToggle = useCallback(async () => {
    if (!definition) return;
    setSaving(true);
    setError(null);
    try {
      const id = await saveAgentDefinition({
        id: definition.id,
        expectedRevision: definition.revision,
        name: definition.name,
        description: definition.description,
        selectedSkillIds: definition.selectedSkillIds,
        selectedMcpServerIds: definition.selectedMcpServerIds,
        providerConnectionIds: definition.providerConnectionIds,
        execution: definition.execution,
        enabled: definition.enabled,
        archived: !definition.archived,
      });
      await onSaved(id, false);
    } catch (err) {
      setError(errText(err, 'Failed to update the agent.'));
    } finally {
      setSaving(false);
    }
  }, [definition, onSaved]);

  const initialAgent: AgentDetail | undefined = definition
    ? {
        id: definition.id,
        name: definition.name,
        description: definition.description,
        enabled: definition.enabled,
        selectedSkillIds: definition.selectedSkillIds,
        selectedMcpServerIds: definition.selectedMcpServerIds,
        providerConnectionIds: definition.providerConnectionIds,
        execution: definition.execution as AgentDetail['execution'],
      }
    : undefined;

  const title = definition ? definition.name : 'New agent';

  return (
    <div className={styles.editor}>
      <nav className={styles.breadcrumb} aria-label="Breadcrumb">
        <button type="button" className={styles.back} onClick={onBack} disabled={saving}>
          ‹ Agents
        </button>
        <span className={styles.crumbSeparator} aria-hidden="true">
          ›
        </span>
        <span className={styles.crumbCurrent}>{title}</span>
      </nav>

      <div className={styles.headerText}>
        <h3 className={styles.title}>{title}</h3>
        <p className={styles.description}>
          {definition
            ? definition.assignedWorkspaces.length > 0
              ? `Saving changes this agent in: ${definition.assignedWorkspaces
                  .map((workspace) => workspace.title)
                  .join(', ')}.`
              : 'Not on any workspace yet. Add it from a workspace’s Agents drawer.'
            : 'Give it a face and a name, then decide what it can do. Workspaces add it from their Agents drawer.'}
        </p>
      </div>

      {error && (
        <div className={styles.errorBanner} role="alert">
          {error}
        </div>
      )}

      <AgentFacePicker
        current={currentIdentity}
        choice={face}
        onChange={setFace}
        disabled={saving}
      />

      <AgentBehaviorForm
        ref={sectionRef}
        workspaceId=""
        agentId={definition?.id ?? null}
        details={null}
        initialAgent={initialAgent}
        saveBehavior={save}
        deps={deps}
        saving={saving}
        onDirtyChange={setDirty}
        showHeading={false}
      />

      <div className={styles.actions}>
        <button type="button" className={styles.primary} onClick={handleSave} disabled={saving}>
          {saving ? 'Saving…' : definition ? 'Save agent' : 'Create agent'}
        </button>
        {definition && (
          <button
            type="button"
            className={styles.secondary}
            onClick={handleArchiveToggle}
            disabled={saving}
          >
            {definition.archived ? 'Restore' : 'Archive'}
          </button>
        )}
        {dirty && !saving && <span className={styles.dirty}>Unsaved changes</span>}
      </div>
    </div>
  );
};

export default AgentEditor;
