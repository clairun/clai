/**
 * The shared agent library.
 *
 * A definition here is behavior without a home: instructions, skills,
 * providers, MCP selection and execution policy, and nothing workspace-shaped —
 * no history, no schedule, no files. It starts working when a workspace puts it
 * on its team, and an edit made here reaches every workspace that did, on their
 * next turn. What stays local to each workspace is its Main agent, edited in
 * that workspace's settings.
 */

import React, { useCallback, useEffect, useRef, useState } from 'react';
import {
  getMcpServers,
  getSkills,
  listAgentDefinitions,
  saveAgentDefinition,
  workspaceAgentDefaultExecution,
  type AgentDefinitionDetail,
} from '../../api/client';
import { assistantClient } from '../../assistant';
import {
  AgentSection,
  type AgentDetail,
  type ModalDeps,
  type SectionHandle,
} from './WorkspaceSettingsModal';
import styles from './AgentLibrarySettings.module.css';

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

/** `null` selection = the "new agent" draft. */
type Selected = { kind: 'new' } | { kind: 'definition'; id: string };

const AgentLibrarySettings = () => {
  const [definitions, setDefinitions] = useState<AgentDefinitionDetail[]>([]);
  const [selected, setSelected] = useState<Selected>({ kind: 'new' });
  const [deps, setDeps] = useState<ModalDeps>({
    mcpServers: [],
    skills: [],
    providerConnections: [],
    defaultExecution: undefined,
  });
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const sectionRef = useRef<SectionHandle | null>(null);

  const reload = useCallback(async () => {
    try {
      const library = await listAgentDefinitions();
      setDefinitions(library);
      return library;
    } catch (err) {
      setError(errText(err, 'Failed to load the agent library.'));
      return [];
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [servers, skills, connections, defaults] = await Promise.allSettled([
        getMcpServers(),
        getSkills(),
        assistantClient.listProviderConnections(),
        workspaceAgentDefaultExecution(),
      ]);
      if (cancelled) return;
      setDeps({
        mcpServers: servers.status === 'fulfilled' ? servers.value || [] : [],
        skills: skills.status === 'fulfilled' ? skills.value || [] : [],
        providerConnections: connections.status === 'fulfilled' ? connections.value || [] : [],
        defaultExecution: defaults.status === 'fulfilled' ? defaults.value || null : null,
      } as ModalDeps);
      await reload();
    })();
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const current =
    selected.kind === 'definition'
      ? definitions.find((definition) => definition.id === selected.id)
      : undefined;

  // The form speaks the workspace-agent shape; the library adds identity and
  // the revision the edit started from.
  const save = useCallback(
    async (payload: Record<string, unknown>, archived: boolean) => {
      const id = await saveAgentDefinition({
        id: current?.id,
        expectedRevision: current?.revision,
        name: payload.name,
        description: payload.description,
        selectedSkillIds: payload.selectedSkillIds,
        selectedMcpServerIds: payload.selectedMcpServerIds,
        providerConnectionIds: payload.providerConnectionIds,
        execution: payload.execution,
        enabled: payload.enabled,
        archived,
      });
      await reload();
      setSelected({ kind: 'definition', id });
    },
    [current, reload]
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
    if (!current) return;
    setSaving(true);
    setError(null);
    try {
      await saveAgentDefinition({
        id: current.id,
        expectedRevision: current.revision,
        name: current.name,
        description: current.description,
        selectedSkillIds: current.selectedSkillIds,
        selectedMcpServerIds: current.selectedMcpServerIds,
        providerConnectionIds: current.providerConnectionIds,
        execution: current.execution,
        enabled: current.enabled,
        archived: !current.archived,
      });
      await reload();
    } catch (err) {
      setError(errText(err, 'Failed to update the agent.'));
    } finally {
      setSaving(false);
    }
  }, [current, reload]);

  const initialAgent: AgentDetail | undefined = current
    ? {
        id: current.id,
        name: current.name,
        description: current.description,
        enabled: current.enabled,
        selectedSkillIds: current.selectedSkillIds,
        selectedMcpServerIds: current.selectedMcpServerIds,
        providerConnectionIds: current.providerConnectionIds,
        execution: current.execution as AgentDetail['execution'],
      }
    : undefined;

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <div className={styles.headerText}>
          <h3 className={styles.title}>Agents</h3>
          <p className={styles.description}>
            Teammates shared across workspaces. Each workspace adds the ones it needs in its own
            settings, and keeps its own Main agent there.
          </p>
        </div>
      </div>

      {error && (
        <div className={styles.errorBanner} role="alert">
          {error}
        </div>
      )}

      <div className={styles.layout}>
        <div className={styles.list}>
          {definitions.map((definition) => (
            <button
              key={definition.id}
              type="button"
              className={`${styles.listItem} ${
                selected.kind === 'definition' && selected.id === definition.id
                  ? styles.listItemActive
                  : ''
              }`}
              onClick={() => setSelected({ kind: 'definition', id: definition.id })}
            >
              <span>
                {definition.name}
                {definition.archived ? ' (archived)' : ''}
              </span>
              <span className={styles.listItemMeta}>
                {definition.assignedWorkspaces.length === 0
                  ? 'Not on any workspace team'
                  : `Used in ${definition.assignedWorkspaces.length} workspace${
                      definition.assignedWorkspaces.length === 1 ? '' : 's'
                    }`}
              </span>
            </button>
          ))}
          <button
            type="button"
            className={`${styles.listItem} ${selected.kind === 'new' ? styles.listItemActive : ''}`}
            onClick={() => setSelected({ kind: 'new' })}
          >
            + New agent
          </button>
        </div>

        <div className={styles.editor}>
          <AgentSection
            // Remount per selection: the form mirrors its agent into local
            // state at load time, so switching agents must start it over.
            key={current?.id || 'new'}
            ref={sectionRef}
            workspaceId=""
            agentId={current?.id ?? null}
            snapshot={null}
            initialAgent={initialAgent}
            saveBehavior={(payload) => save(payload, current?.archived ?? false)}
            deps={deps}
            saving={saving}
          />

          {current && current.assignedWorkspaces.length > 0 && (
            <p className={styles.description}>
              Saving changes this agent in:{' '}
              {current.assignedWorkspaces.map((workspace) => workspace.title).join(', ')}.
            </p>
          )}

          <div className={styles.actions}>
            <button type="button" onClick={handleSave} disabled={saving}>
              {saving ? 'Saving…' : current ? 'Save agent' : 'Create agent'}
            </button>
            {current && (
              <button type="button" onClick={handleArchiveToggle} disabled={saving}>
                {current.archived ? 'Restore' : 'Archive'}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};

export default AgentLibrarySettings;
