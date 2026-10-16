/**
 * Workspace team settings.
 *
 * The split mirrors the backend: a crew member's behavior — instructions,
 * skills, providers, MCP, shell policy — belongs to the shared definition and
 * is edited once, in Settings → Agents. What lives here is only what one
 * project may decide on its own: whether an agent is on, the context it works
 * under, and the paths it may touch in this workspace.
 */

import React, { useCallback, useEffect, useImperativeHandle, useState } from 'react';
import {
  configureWorkspaceAssignment,
  getWorkspaceTeamPolicy,
  listAgentDefinitions,
  saveWorkspaceTeamPolicy,
  workspaceDeleteAgent,
  type AgentDefinitionDetail,
  type PathGrantPayload,
  type WorkspaceAssignmentPayload,
} from '../../api/client';
import AgentCardPicker from '../Agents/AgentCardPicker';
import { openGlobalSettings } from '../../utils/globalSettings';
import { useReportDirty, type SectionHandle } from './sectionHandle';
import { PathGrantEditor, Switch } from './ListEditors';
import form from './SettingsForm.module.css';
import styles from './WorkspaceTeamSettings.module.css';

interface PageProps {
  /** The modal shell saves the page through this handle. */
  ref?: React.Ref<SectionHandle>;
  /** True while the shell is saving this page. */
  saving?: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
}

/**
 * Project context and the access every agent in this workspace receives.
 * Both apply on the next turn, like every other configuration edit.
 */
export const TeamPolicySection = ({
  ref,
  workspaceId,
  saving = false,
  onDirtyChange,
}: PageProps & { workspaceId: string }) => {
  const [context, setContext] = useState('');
  const [grants, setGrants] = useState<PathGrantPayload[]>([]);
  // What was last loaded or saved; the draft is dirty when it differs.
  // Null until the policy loads: there is nothing to edit before that.
  const [baseline, setBaseline] = useState<{ context: string; grants: PathGrantPayload[] } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getWorkspaceTeamPolicy(workspaceId)
      .then((policy) => {
        if (cancelled) return;
        const loaded = { context: policy.context || '', grants: policy.filesystemGrants || [] };
        setContext(loaded.context);
        setGrants(loaded.grants);
        setBaseline(loaded);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  const isDirty = !!baseline
    && (context !== baseline.context || JSON.stringify(grants) !== JSON.stringify(baseline.grants));
  useReportDirty(isDirty, onDirtyChange);

  useImperativeHandle(ref, () => ({
    validate: () => ({ ok: true }),
    submit: async () => {
      // Saving blanks over a policy that never loaded would erase it.
      if (!baseline) return { ok: false, error: error || 'The team policy has not loaded.' };
      try {
        await saveWorkspaceTeamPolicy(workspaceId, context, grants);
        setBaseline({ context, grants });
        setError(null);
        return { ok: true };
      } catch (e: unknown) {
        const message = e instanceof Error ? e.message : String(e);
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  if (!baseline) {
    if (!error) return <PageSkeleton />;
    return (
      <div className={form.page}>
        <p className={form.errorBanner} role="alert">{error}</p>
      </div>
    );
  }

  return (
    <div className={form.page}>
      <div className={form.columns}>
        <section className={`${form.panel} ${form.panelFill}`}>
          <header className={form.panelHeader}>
            <h3 className={form.panelTitle} id="team-context-title">Project context</h3>
            <span className={form.panelMeta}>Added to every agent&apos;s instructions here</span>
          </header>
          <div className={form.panelBody}>
            <p className={form.lede}>
              What is true of the project rather than of one agent. The Main and every crew member get it.
            </p>
            <textarea
              id="workspace-context"
              className={`${form.textarea} ${form.textareaFill}`}
              value={context}
              onChange={(e) => setContext(e.target.value)}
              aria-labelledby="team-context-title"
              placeholder="What this workspace is for, where the code lives, house rules…"
              disabled={saving}
            />
          </div>
        </section>

        <section className={`${form.panel} ${form.panelFill}`}>
          <header className={form.panelHeader}>
            <h3 className={form.panelTitle} id="team-grants-title">Workspace path grants</h3>
            <span className={form.panelMeta}>{grants.length ? `${grants.length} granted` : 'None'}</span>
          </header>
          <div className={form.panelBody}>
            <p className={form.lede}>
              Every agent here gets these on top of its own. Access approved during a run is saved on the
              agent that asked for it, not here.
            </p>
            <PathGrantEditor
              grants={grants}
              onChange={setGrants}
              make={(path, access) => ({ path, access })}
              disabled={saving}
              label="Workspace path grants"
            />
          </div>
        </section>
      </div>

      {error && <p className={form.errorBanner} role="alert">{error}</p>}
    </div>
  );
};

const PageSkeleton = () => (
  <div className={form.page} aria-busy="true" aria-label="Loading">
    <div className={form.skeleton}><span /><span /><span /></div>
  </div>
);

/**
 * Either the editor for one crew member (`agentId` given), or the picker that
 * adds one from the shared library.
 */
export const AssignmentSection = ({
  ref,
  workspaceId,
  agentId,
  saving = false,
  onDirtyChange,
  onChanged,
  onUnassigned,
}: PageProps & {
  workspaceId: string;
  agentId?: string;
  onChanged?: () => void;
  /** Called after a successful unassign so the modal can leave the dead row. */
  onUnassigned?: (workspaceAgentId: string) => void;
}) => {
  const [definitions, setDefinitions] = useState<AgentDefinitionDetail[]>([]);
  const [assignments, setAssignments] = useState<WorkspaceAssignmentPayload[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Local draft for the assignment being edited.
  const [enabled, setEnabled] = useState(true);
  const [context, setContext] = useState('');
  const [grants, setGrants] = useState<PathGrantPayload[]>([]);

  useEffect(() => {
    let cancelled = false;
    Promise.all([listAgentDefinitions(), getWorkspaceTeamPolicy(workspaceId)])
      .then(([library, policy]) => {
        if (cancelled) return;
        setDefinitions(library);
        setAssignments(policy.assignments || []);
        const current = (policy.assignments || []).find((item) => item.id === agentId);
        if (current) {
          setEnabled(current.enabled);
          setContext(current.context || '');
          setGrants(current.filesystemGrants || []);
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, agentId]);

  // Re-reads the library and the roster without touching the draft being
  // edited. Awaited by callers that must not act on the old roster.
  const refresh = useCallback(async () => {
    try {
      const [library, policy] = await Promise.all([listAgentDefinitions(), getWorkspaceTeamPolicy(workspaceId)]);
      setDefinitions(library);
      setAssignments(policy.assignments || []);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [workspaceId]);

  const assignment = assignments.find((item) => item.id === agentId);
  const definition = definitions.find((item) => item.id === assignment?.agentDefinitionId);
  const inert = busy || saving;

  const isDirty = !!assignment && (
    enabled !== assignment.enabled
    || context !== (assignment.context || '')
    || JSON.stringify(grants) !== JSON.stringify(assignment.filesystemGrants || [])
  );
  useReportDirty(isDirty, onDirtyChange);

  // The picker assigns on its own and stays busy until this resolves, so the
  // roster it filters on is the new one before another card can be clicked.
  const handleCrewChanged = useCallback(async () => {
    await refresh();
    onChanged?.();
  }, [refresh, onChanged]);

  // The picker has no draft: its assign is immediate and offers its own Undo.
  useImperativeHandle(ref, () => ({
    validate: () => ({ ok: true }),
    submit: async () => {
      if (!assignment) return { ok: true };
      const next = { ...assignment, enabled, context, filesystemGrants: grants };
      try {
        await configureWorkspaceAssignment(workspaceId, next);
        // The saved row is the new baseline, so the page reads clean.
        setAssignments((rows) => rows.map((row) => (row.id === next.id ? next : row)));
        setError(null);
        return { ok: true };
      } catch (e: unknown) {
        const message = e instanceof Error ? e.message : String(e);
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  const handleUnassign = useCallback(async () => {
    if (!agentId) return;
    // Unlike unchecking Enabled, this drops the row and everything hanging off
    // it. Confirming matches the other destructive action in this modal
    // (AgentBehaviorForm's "Delete agent").
    const name = definition?.name || 'this agent';
    if (
      !window.confirm(
        `Remove ${name} from this crew? The context and path grants it has here are deleted, and adding it back gives it a new callable id.`
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, agentId);
      onChanged?.();
      // The row this section was editing is gone; the modal navigates away
      // rather than leaving a pane of disabled controls on a dead id.
      onUnassigned?.(agentId);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [workspaceId, agentId, definition?.name, onChanged, onUnassigned]);

  // The library opens over this modal, which stays open underneath with its
  // draft intact — nothing to warn about.
  const handleOpenDefinition = useCallback(() => {
    if (!definition) return;
    openGlobalSettings({ tab: 'agents', agentDefinitionId: definition.id });
  }, [definition]);

  // Whether this agent runs is three switches ANDed together (see
  // config/global_agents.rs): the checkbox below, the definition's own
  // Enabled, and the definition not being archived. This pane owns the first
  // one only, so it says when one of the other two is holding the agent off.
  const libraryHold = !definition
    ? null
    : definition.archived
      ? 'archived'
      : definition.enabled
        ? null
        : 'disabled';

  if (loading) return <PageSkeleton />;

  // Picker: every definition the library offers that this workspace has not
  // already assigned. Assigning the same one twice would produce two local ids
  // with identical behavior and no way to tell them apart.
  if (!agentId) {
    return (
      <div className={form.page}>
        {error && <p className={form.errorBanner} role="alert">{error}</p>}
        <AgentCardPicker
          workspaceId={workspaceId}
          definitions={definitions}
          assignedDefinitionIds={assignments.map((item) => item.agentDefinitionId)}
          onChanged={handleCrewChanged}
          // The global Settings modal stacks above this one (see the
          // --z-portal-* scale), so the library opens on top and this window
          // is waiting underneath when it closes.
          onCreateAgent={() => openGlobalSettings({ tab: 'agents' })}
          disabled={busy}
        />
      </div>
    );
  }

  if (!assignment) {
    return (
      <div className={form.page}>
        <p className={form.errorBanner} role="alert">This agent is no longer on the crew.</p>
      </div>
    );
  }

  const name = definition?.name || 'Unavailable agent';

  return (
    <div className={form.page}>
      <div className={styles.identity}>
        <p className={form.metaRow}>
          <span>Callable id</span>
          <code className={form.code}>{assignment.id}</code>
        </p>
        <div className={styles.enabled}>
          <Switch
            label="Enabled in this workspace"
            checked={enabled}
            onChange={setEnabled}
            disabled={inert}
          />
        </div>
      </div>

      {definition ? (
        <p className={styles.shared}>
          Instructions, skills, model, MCP and shell come from {name}&apos;s shared definition, and an
          edit there reaches every workspace using it.{' '}
          <button type="button" className={form.linkButton} onClick={handleOpenDefinition}>
            Edit in Settings → Agents
          </button>
        </p>
      ) : (
        <p className={form.hintWarning}>
          This assignment points at a shared agent that no longer exists. Re-create it in Settings →
          Agents, or remove it from the crew.
        </p>
      )}
      {libraryHold && (
        <p className={form.hintWarning} role="note">
          {name} is {libraryHold} in the library, so it stays off here whatever this switch says.
          Change that on the shared definition.
        </p>
      )}
      {!enabled && (
        <p className={form.hint}>
          Off parks it: it keeps its seat, context and grants, and it is still listed to other agents
          (though <code className={form.code}>workspace_listAgents</code> skips it by default). Tasks
          delegated to it are refused.
        </p>
      )}

      <div className={form.columns}>
        <section className={`${form.panel} ${form.panelFill}`}>
          <header className={form.panelHeader}>
            <h3 className={form.panelTitle} id="assignment-context-title">Context for this workspace</h3>
            <span className={form.panelMeta}>Added to its shared instructions, here only</span>
          </header>
          <div className={form.panelBody}>
            <textarea
              id="assignment-context"
              className={`${form.textarea} ${form.textareaFill}`}
              value={context}
              onChange={(e) => setContext(e.target.value)}
              aria-labelledby="assignment-context-title"
              placeholder="What this agent should know about its job here."
              disabled={inert}
            />
          </div>
        </section>

        <section className={`${form.panel} ${form.panelFill}`}>
          <header className={form.panelHeader}>
            <h3 className={form.panelTitle} id="assignment-grants-title">Path grants in this workspace</h3>
          </header>
          <div className={form.panelBody}>
            <p className={form.lede}>
              Local to this workspace. Paths approved during a run land here too, so approving access in
              one project never widens it in another.
            </p>
            <PathGrantEditor
              grants={grants}
              onChange={setGrants}
              make={(path, access) => ({ path, access })}
              disabled={inert}
              label="Path grants in this workspace"
            />
          </div>
        </section>
      </div>

      {error && <p className={form.errorBanner} role="alert">{error}</p>}

      <div className={styles.danger}>
        <button type="button" className={form.dangerButton} onClick={handleUnassign} disabled={inert}>
          Remove from crew
        </button>
        <span className={form.hint}>
          Deletes the context and grants above and retires the callable id. Switch it off instead to park it.
        </span>
      </div>
    </div>
  );
};
