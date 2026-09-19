/**
 * Workspace team settings.
 *
 * The split mirrors the backend: a crew member's behavior — instructions,
 * skills, providers, MCP, shell policy — belongs to the shared definition and
 * is edited once, in Settings → Agents. What lives here is only what one
 * project may decide on its own: whether an agent is on, the context it works
 * under, and the paths it may touch in this workspace.
 */

import React, { useCallback, useEffect, useState } from 'react';
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
import styles from './WorkspaceSettingsModal.module.css';

/** Editable list of path grants, shared by the assignment and policy forms. */
export const PathGrantList = ({
  grants,
  onChange,
  disabled,
  placeholder = '/home/user/project',
}: {
  grants: PathGrantPayload[];
  onChange: (next: PathGrantPayload[]) => void;
  disabled?: boolean;
  placeholder?: string;
}) => {
  const [draft, setDraft] = useState('');
  const [access, setAccess] = useState('read_only');

  const add = () => {
    const path = draft.trim();
    if (!path || grants.some((grant) => grant.path === path)) {
      setDraft('');
      return;
    }
    onChange([...grants, { path, access }]);
    setDraft('');
  };

  return (
    <>
      {grants.length > 0 && (
        <div className={styles.grantList}>
          {grants.map((grant) => (
            <div key={grant.path} className={styles.grantItem}>
              <span className={styles.grantPath}>{grant.path}</span>
              <span className={styles.grantAccess}>
                {grant.access === 'read_write' ? 'RW' : 'RO'}
              </span>
              <button
                type="button"
                className={styles.chipRemove}
                onClick={() => onChange(grants.filter((item) => item.path !== grant.path))}
                disabled={disabled}
                aria-label={`Remove grant for ${grant.path}`}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
      <div className={styles.listInputRow}>
        <input
          type="text"
          className={styles.input}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={placeholder}
          disabled={disabled}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              add();
            }
          }}
        />
        <select
          className={styles.select}
          value={access}
          onChange={(e) => setAccess(e.target.value)}
          disabled={disabled}
        >
          <option value="read_only">Read only</option>
          <option value="read_write">Read &amp; write</option>
        </select>
        <button type="button" className={styles.addButton} onClick={add} disabled={disabled || !draft.trim()}>
          Add
        </button>
      </div>
    </>
  );
};

/**
 * Project context and the access every agent in this workspace receives.
 * Both apply on the next turn, like every other configuration edit.
 */
export const TeamPolicySection = ({
  workspaceId,
  onChanged,
}: {
  workspaceId: string;
  onChanged?: () => void;
}) => {
  const [context, setContext] = useState('');
  const [grants, setGrants] = useState<PathGrantPayload[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // `loading` starts true and is only ever cleared here, so the effect never
  // sets state synchronously on the way in.
  useEffect(() => {
    let cancelled = false;
    getWorkspaceTeamPolicy(workspaceId)
      .then((policy) => {
        if (cancelled) return;
        setContext(policy.context || '');
        setGrants(policy.filesystemGrants || []);
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
  }, [workspaceId]);

  const save = useCallback(async () => {
    setSaving(true);
    setError(null);
    try {
      await saveWorkspaceTeamPolicy(workspaceId, context, grants);
      setSaved(true);
      onChanged?.();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [workspaceId, context, grants, onChanged]);

  if (loading) return <div className={styles.sectionRoot}>Loading…</div>;

  return (
    <div className={styles.sectionRoot}>
      <h4 className={styles.sectionTitle}>Project context</h4>
      <p className={styles.sectionDescription}>
        Added to the instructions of every agent working in this workspace — the Main and each
        crew member. Use it for what is true of the project, not of any one agent.
      </p>
      <div className={styles.field}>
        <label className={styles.label} htmlFor="workspace-context">
          Instructions shared by every agent here
        </label>
        <textarea
          id="workspace-context"
          className={styles.textarea}
          value={context}
          onChange={(e) => {
            setContext(e.target.value);
            setSaved(false);
          }}
          rows={6}
          placeholder="What this workspace is for, where the code lives, house rules…"
          disabled={saving}
        />
      </div>

      <h4 className={styles.sectionTitle} style={{ marginTop: 24 }}>
        Workspace path grants
      </h4>
      <p className={styles.sectionDescription}>
        Granted to every agent here, on top of its own. Access approved in a run is saved on the
        agent that asked for it, not here.
      </p>
      <div className={styles.field}>
        <PathGrantList
          grants={grants}
          onChange={(next) => {
            setGrants(next);
            setSaved(false);
          }}
          disabled={saving}
        />
      </div>

      {error && (
        <div className={styles.errorBanner} role="alert">
          {error}
        </div>
      )}
      <div className={styles.actions}>
        <button type="button" className={styles.primaryButton} onClick={save} disabled={saving}>
          {saving ? 'Saving…' : saved ? 'Saved' : 'Save team settings'}
        </button>
      </div>
    </div>
  );
};

/**
 * Either the editor for one crew member (`agentId` given), or the picker that
 * adds one from the shared library.
 */
export const AssignmentSection = ({
  workspaceId,
  agentId,
  onChanged,
  onUnassigned,
}: {
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

  // The picker assigns on its own and stays busy until this resolves, so the
  // roster it filters on is the new one before another card can be clicked.
  const handleCrewChanged = useCallback(async () => {
    await refresh();
    onChanged?.();
  }, [refresh, onChanged]);

  const handleSave = useCallback(async () => {
    if (!assignment) return;
    setBusy(true);
    setError(null);
    try {
      await configureWorkspaceAssignment(workspaceId, {
        ...assignment,
        enabled,
        context,
        filesystemGrants: grants,
      });
      await refresh();
      onChanged?.();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [workspaceId, assignment, enabled, context, grants, refresh, onChanged]);

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

  if (loading) return <div className={styles.sectionRoot}>Loading…</div>;

  // Picker: every definition the library offers that this workspace has not
  // already assigned. Assigning the same one twice would produce two local ids
  // with identical behavior and no way to tell them apart.
  if (!agentId) {
    return (
      <div className={styles.sectionRoot}>
        <h4 className={styles.sectionTitle}>Add to crew</h4>
        {error && (
          <div className={styles.errorBanner} role="alert">
            {error}
          </div>
        )}
        <AgentCardPicker
          workspaceId={workspaceId}
          definitions={definitions}
          assignedDefinitionIds={assignments.map((item) => item.agentDefinitionId)}
          onChanged={handleCrewChanged}
          // The global Settings modal stacks above this one (see the
          // --z-portal-* scale), so the library opens on top and this modal is
          // waiting underneath when it closes.
          onCreateAgent={() => openGlobalSettings({ tab: 'agents' })}
          disabled={busy}
        />
      </div>
    );
  }

  if (!assignment) {
    return (
      <div className={styles.sectionRoot}>
        <div className={styles.errorBanner}>This agent is no longer on the crew.</div>
      </div>
    );
  }

  return (
    <div className={styles.sectionRoot}>
      <h4 className={styles.sectionTitle}>{definition?.name || 'Unavailable agent'}</h4>
      <p className={styles.metaRow}>
        <span>Callable id</span>
        <code>{assignment.id}</code>
      </p>
      {definition ? (
        <div className={styles.splitNote}>
          <p>
            <strong>Set here</strong>: whether {definition.name} is on in this workspace, the
            context it works under, and the paths it may touch — the three things below.
          </p>
          <p>
            <strong>Shared everywhere</strong>: instructions, skills, providers, MCP and shell
            policy. Those belong to the agent&apos;s definition, and an edit there reaches every
            workspace using it, from its next turn. A definition that is disabled or archived
            there stays off in every workspace, including this one.{' '}
            <button type="button" className={styles.linkButton} onClick={handleOpenDefinition}>
              Edit in Settings → Agents
            </button>
          </p>
        </div>
      ) : (
        <p className={styles.sectionDescription}>
          This assignment points at a shared agent that no longer exists. Re-create it in Settings
          → Agents, or remove it from the crew.
        </p>
      )}

      <div className={styles.field}>
        <label className={styles.checkboxRow}>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            disabled={busy}
          />
          <span>Enabled in this workspace</span>
        </label>
        <span className={styles.hint}>
          Off parks it here: it stays on the crew with the context and grants below, and other
          agents still see it listed, but it does not run and delegating a task to it is refused.
          Reversible at any time.
        </span>
        {libraryHold && (
          <span className={styles.hintWarning}>
            {definition?.name} is {libraryHold} in the library, so it stays off here whatever this
            box says — change that on the shared definition.
          </span>
        )}
      </div>

      <div className={styles.field}>
        <label className={styles.label} htmlFor="assignment-context">
          Context for this workspace
        </label>
        <textarea
          id="assignment-context"
          className={styles.textarea}
          value={context}
          onChange={(e) => setContext(e.target.value)}
          rows={5}
          placeholder="What this agent should know about its job here."
          disabled={busy}
        />
        <span className={styles.hint}>
          Added to the shared instructions, in this workspace only.
        </span>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Path grants in this workspace</label>
        <PathGrantList grants={grants} onChange={setGrants} disabled={busy} />
        <span className={styles.hint}>
          Local to this workspace. Paths approved during a run are saved here too, so approving
          access in one project never widens it in another.
        </span>
      </div>

      {error && (
        <div className={styles.errorBanner} role="alert">
          {error}
        </div>
      )}
      <div className={styles.actions}>
        <div className={styles.dangerZone}>
          <button
            type="button"
            className={styles.dangerButton}
            onClick={handleUnassign}
            disabled={busy}
          >
            Remove from crew
          </button>
          <span className={styles.hint}>
            Deletes the context and grants above and retires the callable id. Uncheck Enabled
            instead to park it.
          </span>
        </div>
        <button type="button" className={styles.primaryButton} onClick={handleSave} disabled={busy}>
          {busy ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  );
};
