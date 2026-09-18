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
    <div>
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
      <button type="button" className={styles.primaryButton} onClick={save} disabled={saving}>
        {saving ? 'Saving…' : saved ? 'Saved' : 'Save team settings'}
      </button>
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
  }, [workspaceId, agentId, onChanged, onUnassigned]);

  if (loading) return <div className={styles.sectionRoot}>Loading…</div>;

  // Picker: every definition the library offers that this workspace has not
  // already assigned. Assigning the same one twice would produce two local ids
  // with identical behavior and no way to tell them apart.
  if (!agentId) {
    return (
      <div>
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
          onCreateAgent={() => openGlobalSettings({ tab: 'agents' })}
          disabled={busy}
        />
      </div>
    );
  }

  if (!assignment) {
    return <div className={styles.errorBanner}>This agent is no longer on the crew.</div>;
  }

  return (
    <div>
      <h4 className={styles.sectionTitle}>{definition?.name || 'Unavailable agent'}</h4>
      <p className={styles.sectionDescription}>
        {definition
          ? 'Shared agent. Instructions, skills, providers, MCP and shell policy are edited once in Settings → Agents and apply in every workspace that uses this agent, from its next turn.'
          : 'This assignment points at a shared agent that no longer exists. Re-create it in Settings → Agents, or remove it from the crew.'}
      </p>
      <p className={styles.sectionDescription}>
        Callable id: <code>{assignment.id}</code>
      </p>

      <div className={styles.field}>
        <label className={styles.label}>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            disabled={busy}
          />{' '}
          Enabled in this workspace
        </label>
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
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Path grants in this workspace</label>
        <p className={styles.sectionDescription}>
          Local to this workspace. Paths approved during a run are saved here too, so approving
          access in one project never widens it in another.
        </p>
        <PathGrantList grants={grants} onChange={setGrants} disabled={busy} />
      </div>

      {error && (
        <div className={styles.errorBanner} role="alert">
          {error}
        </div>
      )}
      <div className={styles.listInputRow}>
        <button type="button" className={styles.primaryButton} onClick={handleSave} disabled={busy}>
          {busy ? 'Saving…' : 'Save'}
        </button>
        <button type="button" className={styles.dangerButton} onClick={handleUnassign} disabled={busy}>
          Remove from crew
        </button>
      </div>
    </div>
  );
};
