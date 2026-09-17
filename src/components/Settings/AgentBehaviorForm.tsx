/**
 * AgentBehaviorForm
 *
 * The one form that edits what an agent *is*: name, description, skills,
 * MCP servers, provider connections and local capabilities. Two hosts
 * render it — the Workspace Settings modal (per-workspace Main / crew
 * member, saves through the workspace commands) and the agent library
 * (shared definition, saves through `saveBehavior`). Both drive it through
 * the imperative `SectionHandle` so a global Save button can validate and
 * submit it without owning its state.
 */

import React, { useState, useEffect, useCallback, useImperativeHandle, useMemo, useRef } from 'react';
import {
  workspaceCreateAgent,
  workspaceDeleteAgent,
  workspaceGetAgent,
  workspaceUpdateAgent,
} from '../../api/client';
import SkillPicker from './SkillPicker';
import type { ProviderConnection, WorkspaceSnapshot } from '../../generated/bindings';
import type { SectionHandle } from './sectionHandle';
// Shares the modal's stylesheet on purpose: the form is rendered inside the
// modal's content pane and must match its fields, toggles and chips exactly.
import styles from './WorkspaceSettingsModal.module.css';

// ──────────────────────────────────────────────────────────────────────────
// Shapes. The execution-config tree and agent payloads are ad-hoc (sourced
// from untyped api/client.js commands); modeled loosely here rather than
// dragging the full config module into the FE types.
// ──────────────────────────────────────────────────────────────────────────

interface GrantOrigin {
  kind: string;
  grantedAtUnixMs?: number;
  reason?: string;
}
interface PathGrant {
  path: string;
  access: string;
  origin?: GrantOrigin | null;
}
interface ExecutionConfig {
  sandbox: { network: string; sessionBus: string };
  filesystem: { extraPaths: PathGrant[] };
  shell: {
    mode: string;
    allowedCommandPrefixes: string[];
    blockedCommandPrefixes: string[];
  };
  web: { enabled: boolean };
}

interface NamedRef {
  id: string;
  name: string;
  description?: string | null;
}
// Skills carry their originating source so the picker can group by it.
interface SkillRef extends NamedRef {
  sourceId?: string | null;
  sourceName?: string | null;
}
/** What the behaviour form submits. The library saves it as a shared definition. */
export interface AgentBehaviorPayload {
  workspaceId: string;
  name: string;
  description: string;
  selectedSkillIds: string[];
  selectedMcpServerIds: string[];
  providerConnectionIds: string[];
  execution: ExecutionConfig;
  enabled: boolean;
}

// Agent detail loaded from workspaceGetAgent (untyped command).
export interface AgentDetail {
  id: string;
  name?: string;
  description?: string;
  isDefault?: boolean;
  enabled?: boolean;
  selectedSkillIds?: string[];
  selectedMcpServerIds?: string[];
  providerConnectionIds?: string[];
  execution?: Partial<ExecutionConfig>;
}

export interface AgentFormDeps {
  mcpServers: NamedRef[];
  skills: SkillRef[];
  providerConnections: ProviderConnection[];
  defaultExecution: Partial<ExecutionConfig> | null | undefined;
}

// ──────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────

const defaultExecution = (): ExecutionConfig => ({
  sandbox: { network: 'enabled', sessionBus: 'allow' },
  filesystem: { extraPaths: [] },
  shell: {
    mode: 'off',
    allowedCommandPrefixes: [],
    blockedCommandPrefixes: [
      'rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot',
    ],
  },
  web: { enabled: false },
});

const normalizeItems = (items: string[] = []): string[] =>
  items.map((item) => item.trim()).filter(Boolean);

const addUniqueItem = (items: string[], value: string): string[] => {
  const trimmed = value.trim();
  if (!trimmed || items.includes(trimmed)) return items;
  return [...items, trimmed];
};

const normalizePathGrants = (items: PathGrant[] = []): PathGrant[] =>
  items
    .map((item) => ({
      path: item.path?.trim() || '',
      access: item.access || 'read_only',
      origin: item.origin || null,
    }))
    .filter((item) => item.path);

const grantOriginLabel = (origin: GrantOrigin | null | undefined): string => {
  if (!origin || origin.kind === 'manual') return 'Manual';
  if (origin.kind === 'credentialsPreset') return 'Preset';
  if (origin.kind === 'approval') {
    const when = origin.grantedAtUnixMs
      ? new Date(origin.grantedAtUnixMs).toLocaleDateString(undefined, {
          year: 'numeric',
          month: 'short',
          day: 'numeric',
        })
      : null;
    return when ? `Approved ${when}` : 'Approved';
  }
  return 'Manual';
};

const normalizeExecution = (execution: Partial<ExecutionConfig> = {}): ExecutionConfig => {
  const d = defaultExecution();
  return {
    sandbox: {
      network: execution.sandbox?.network || d.sandbox.network,
      sessionBus: execution.sandbox?.sessionBus || d.sandbox.sessionBus,
    },
    filesystem: {
      extraPaths: normalizePathGrants(execution.filesystem?.extraPaths || d.filesystem.extraPaths),
    },
    shell: {
      mode: execution.shell?.mode || d.shell.mode,
      allowedCommandPrefixes: normalizeItems(
        execution.shell?.allowedCommandPrefixes || d.shell.allowedCommandPrefixes
      ),
      blockedCommandPrefixes: normalizeItems(execution.shell?.blockedCommandPrefixes || d.shell.blockedCommandPrefixes),
    },
    web: { enabled: execution.web?.enabled || false },
  };
};

// Canonical JSON of the editable agent fields. Used to compare current
// form state against the loaded baseline so the Save button can disable
// itself when there are no pending changes. Pure function — pass already
// normalized values (e.g., execution coming from normalizeExecution).
interface AgentPayloadInput {
  name?: string;
  description?: string;
  selectedSkillIds?: string[];
  selectedMcpServerIds?: string[];
  providerConnectionIds?: string[];
  sessionBusAllowed?: boolean;
  extraPathGrants?: PathGrant[];
  shellMode?: string;
  allowedCommands?: string[];
  blockedCommands?: string[];
  webEnabled?: boolean;
  enabled?: boolean;
}

const serializeAgentPayload = ({
  name,
  description,
  selectedSkillIds,
  selectedMcpServerIds,
  providerConnectionIds,
  sessionBusAllowed,
  extraPathGrants,
  shellMode,
  allowedCommands,
  blockedCommands,
  webEnabled,
  enabled,
}: AgentPayloadInput): string => JSON.stringify({
  name: (name || '').trim(),
  description: (description || '').trim(),
  selectedSkillIds: [...(selectedSkillIds || [])],
  selectedMcpServerIds: [...(selectedMcpServerIds || [])],
  providerConnectionIds: [...(providerConnectionIds || [])],
  execution: {
    sandbox: { sessionBus: sessionBusAllowed ? 'allow' : 'deny' },
    filesystem: { extraPaths: extraPathGrants || [] },
    shell: {
      mode: shellMode,
      allowedCommandPrefixes: allowedCommands || [],
      blockedCommandPrefixes: blockedCommands || [],
    },
    web: { enabled: !!webEnabled },
  },
  enabled: !!enabled,
});

export const AgentBehaviorForm = ({
  ref,
  workspaceId,
  agentId,             // string for edit; null for create
  initialAgent,
  saveBehavior,
  snapshot: _snapshot, // unused; kept in signature for future use (e.g., showing peer agents)
  deps,
  saving,              // global save in flight — disables inputs
  onDirtyChange,
  onDeleted,
}: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  agentId: string | null;
  snapshot: WorkspaceSnapshot | null;
  initialAgent?: AgentDetail;
  saveBehavior?: (payload: AgentBehaviorPayload) => Promise<void>;
  deps: AgentFormDeps;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
  onDeleted?: () => void;
}) => {
  const isCreate = !agentId;
  // Both flows start in a loading state: edit waits on `workspaceGetAgent`,
  // create waits on `deps.defaultExecution` so the form opens with the
  // backend's `$HOME` RO grant pre-populated instead of an empty list.
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Local busy flag for the Delete imperative action. Save goes through
  // the parent's `saving` prop.
  const [deleting, setDeleting] = useState(false);
  const busy = saving || deleting;

  // Source-of-truth agent payload (loaded for edit, blank draft for create
  // — populated once `deps.defaultExecution` arrives).
  const [agent, setAgent] = useState<AgentDetail | null>(null);
  const isManager = agent?.isDefault === true;
  const canDelete = !saveBehavior && !isCreate && !isManager;

  // Form state
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [selectedMcpServerIds, setSelectedMcpServerIds] = useState<string[]>([]);
  const [selectedSkillIds, setSelectedSkillIds] = useState<string[]>([]);
  const [providerConnectionIds, setProviderConnectionIds] = useState<string[]>([]);
  const [providerConnectionDraft, setProviderConnectionDraft] = useState('');
  const [extraPathGrants, setExtraPathGrants] = useState<PathGrant[]>([]);
  const [extraPathDraft, setExtraPathDraft] = useState('');
  const [extraPathAccess, setExtraPathAccess] = useState('read_only');
  const [sessionBusAllowed, setSessionBusAllowed] = useState(true);
  const [shellMode, setShellMode] = useState('off');
  const [allowedCommands, setAllowedCommands] = useState<string[]>([]);
  const [blockedCommands, setBlockedCommands] = useState(defaultExecution().shell.blockedCommandPrefixes);
  const [allowedCommandDraft, setAllowedCommandDraft] = useState('');
  const [blockedCommandDraft, setBlockedCommandDraft] = useState('');
  const [webEnabled, setWebEnabled] = useState(false);
  const [enabled, setEnabled] = useState(true);

  // Track which agentId we last fetched, so the effect doesn't refetch on
  // every re-render. setting key={agentId} on the parent already remounts
  // but this is a belt-and-braces for future callers.
  const lastFetchedId = useRef<string | null>(null);

  // Baseline payload captured at load time. The Save button compares
  // current form state against this to decide whether anything is pending.
  // Updated after a successful save so Save re-disables until the user
  // edits again.
  const baselinePayloadRef = useRef<string | null>(null);

  // Load the agent for the edit flow.
  useEffect(() => {
    if (isCreate) return undefined;
    if (lastFetchedId.current === agentId) return undefined;
    let cancelled = false;
    setLoading(true);
    setError(null);
    (initialAgent ? Promise.resolve(initialAgent) : workspaceGetAgent(workspaceId, agentId))
      .then((detail) => {
        if (cancelled || !detail) return;
        lastFetchedId.current = agentId;
        setAgent(detail as unknown as AgentDetail);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load agent.');
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [workspaceId, agentId, isCreate, initialAgent]);

  // Initialize the blank draft for create flow once the backend's
  // default-execution fetch has resolved (success or failure). Doing this
  // here — rather than in the useState initializer — means the form opens
  // with `$HOME` RO already showing in the path-grants list, which the
  // user can ×-remove before clicking Create. A failed fetch falls back
  // to the local empty execution so the create flow still works.
  useEffect(() => {
    if (!isCreate) return;
    if (agent) return;                              // already initialized
    if (deps?.defaultExecution === undefined) return; // fetch still pending
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Bootstraps the create-flow draft from the resolved defaultExecution; the lint cannot model the fetch-pending guard (deps?.defaultExecution === undefined).
    setAgent(blankAgentDraft(deps.defaultExecution || undefined));
    setLoading(false);
  }, [isCreate, agent, deps?.defaultExecution]);

  // Reset form fields whenever the source agent changes
  useEffect(() => {
    if (!agent) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Resets all 15+ form fields when the source agent changes; the lint cannot model a multi-field prop→state mirror without causing cascading renders on every input.
    setName(agent.name || '');
    setDescription(agent.description || '');
    setSelectedMcpServerIds(agent.selectedMcpServerIds || []);
    setSelectedSkillIds(agent.selectedSkillIds || []);
    setProviderConnectionIds(agent.providerConnectionIds || []);
    setProviderConnectionDraft('');
    const execution = normalizeExecution(agent.execution);
    setExtraPathGrants(execution.filesystem.extraPaths);
    setExtraPathDraft('');
    setExtraPathAccess('read_only');
    setSessionBusAllowed(execution.sandbox.sessionBus === 'allow');
    setShellMode(execution.shell.mode);
    setAllowedCommands(execution.shell.allowedCommandPrefixes);
    setBlockedCommands(execution.shell.blockedCommandPrefixes);
    setAllowedCommandDraft('');
    setBlockedCommandDraft('');
    setWebEnabled(execution.web.enabled);
    setEnabled(agent.enabled !== false);

    // Capture the baseline that matches the values we just loaded into
    // form state. Built from the same normalized `execution` so the
    // representation matches what `currentPayload` will produce.
    baselinePayloadRef.current = serializeAgentPayload({
      name: agent.name,
      description: agent.description,
      selectedSkillIds: agent.selectedSkillIds,
      selectedMcpServerIds: agent.selectedMcpServerIds,
      providerConnectionIds: agent.providerConnectionIds,
      sessionBusAllowed: execution.sandbox.sessionBus === 'allow',
      extraPathGrants: execution.filesystem.extraPaths,
      shellMode: execution.shell.mode,
      allowedCommands: execution.shell.allowedCommandPrefixes,
      blockedCommands: execution.shell.blockedCommandPrefixes,
      webEnabled: execution.web.enabled,
      enabled: agent.enabled !== false,
    });
  }, [agent]);

  const enabledProviderConnections = useMemo(
    () => (deps?.providerConnections || []).filter((c) => c.enabled),
    [deps?.providerConnections]
  );

  const availableProviderConnections = useMemo(
    () => enabledProviderConnections.filter((c) => !providerConnectionIds.includes(c.id)),
    [enabledProviderConnections, providerConnectionIds]
  );

  useEffect(() => {
    if (providerConnectionDraft && availableProviderConnections.some((c) => c.id === providerConnectionDraft)) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Auto-selects the first available provider connection when the list changes; the lint cannot model a "keep current if still valid, otherwise default" derivation.
    setProviderConnectionDraft(availableProviderConnections[0]?.id || '');
  }, [availableProviderConnections, providerConnectionDraft]);

  // Canonical form payload, used to detect pending changes.
  const currentPayload = useMemo(
    () => serializeAgentPayload({
      name,
      description,
      selectedSkillIds,
      selectedMcpServerIds,
      providerConnectionIds,
      sessionBusAllowed,
      extraPathGrants,
      shellMode,
      allowedCommands,
      blockedCommands,
      webEnabled,
      enabled,
    }),
    [
      name, description, selectedSkillIds, selectedMcpServerIds, providerConnectionIds,
      sessionBusAllowed, extraPathGrants, shellMode, allowedCommands, blockedCommands,
      webEnabled, enabled,
    ]
  );

  // True when the form has changed from the loaded (or freshly created)
  // baseline. Drives the Save button's enabled state.
  // `baselinePayloadRef` is a load/save snapshot, not a measurement
  // cache: it is written once on agent load and once after a successful
  // save (see the load effect above and the save handler below). It
  // intentionally lives in a ref so that a re-render triggered by an
  // edit does not produce a fresh baseline. Reading it here is the only
  // way to derive isDirty in render without a state mirror and the
  // accompanying render-stale flash.
  // eslint-disable-next-line react-hooks/refs -- see justification above
  const isDirty = baselinePayloadRef.current !== null
    // eslint-disable-next-line react-hooks/refs -- see justification above
    && baselinePayloadRef.current !== currentPayload;

  const handleAddAllowedCommand = () => {
    const prefix = allowedCommandDraft.trim();
    if (!prefix) return;
    setAllowedCommands((s) => addUniqueItem(s, prefix));
    setAllowedCommandDraft('');
  };

  const handleAddBlockedCommand = () => {
    const prefix = blockedCommandDraft.trim();
    if (!prefix) return;
    setBlockedCommands((s) => addUniqueItem(s, prefix));
    setBlockedCommandDraft('');
  };

  const handleAddPathGrant = () => {
    const path = extraPathDraft.trim();
    if (!path) return;
    if (extraPathGrants.some((g) => g.path === path)) {
      setExtraPathDraft('');
      return;
    }
    setExtraPathGrants((current) => [
      ...current,
      { path, access: extraPathAccess, origin: null },
    ]);
    setExtraPathDraft('');
  };

  // Validation rules surfaced both as an imperative `validate()` and from
  // inside `submit()`. Keeping them in one place avoids drift when we add
  // a new rule.
  const validateAgent = useCallback(() => {
    const trimmedName = name.trim();
    if (!isManager && !trimmedName) {
      return { ok: false, error: 'Agent name is required.' };
    }
    if (trimmedName.length > 100) {
      return { ok: false, error: 'Name must be 100 characters or less.' };
    }
    if (providerConnectionIds.length === 0) {
      return { ok: false, error: 'Select at least one provider connection.' };
    }
    return { ok: true };
  }, [name, isManager, providerConnectionIds]);

  // Report dirty changes up to the modal so the global Save button and
  // the sidebar dot indicators reflect current state.
  const onDirtyChangeRef = useRef(onDirtyChange);
  useEffect(() => { onDirtyChangeRef.current = onDirtyChange; });
  // eslint-disable-next-line react-hooks/refs
  useEffect(() => { onDirtyChangeRef.current?.(isDirty); }, [isDirty]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const v = validateAgent();
      if (!v.ok) setError(v.error ?? null);
      else setError(null);
      return v;
    },
    submit: async () => {
      const v = validateAgent();
      if (!v.ok) {
        setError(v.error ?? null);
        return v;
      }
      const trimmedName = name.trim();
      const execution = {
        sandbox: {
          network: 'enabled',
          sessionBus: sessionBusAllowed ? 'allow' : 'deny',
        },
        filesystem: { extraPaths: extraPathGrants },
        shell: {
          mode: shellMode,
          allowedCommandPrefixes: allowedCommands,
          blockedCommandPrefixes: blockedCommands,
        },
        web: { enabled: webEnabled },
      };
      try {
        if (saveBehavior) {
          await saveBehavior({ workspaceId, name: trimmedName, description: description.trim(), selectedSkillIds, selectedMcpServerIds, providerConnectionIds, execution, enabled });
          baselinePayloadRef.current = currentPayload;
        } else if (isCreate) {
          await workspaceCreateAgent({
            workspaceId,
            name: trimmedName,
            description: description.trim(),
            selectedSkillIds,
            selectedMcpServerIds,
            providerConnectionIds,
            execution,
            enabled,
          });
        } else {
          await workspaceUpdateAgent({
            workspaceId,
            agentId: agent?.id,
            name: isManager ? (agent?.name || 'Manager') : trimmedName,
            description: description.trim(),
            selectedSkillIds,
            selectedMcpServerIds,
            providerConnectionIds,
            execution,
            enabled: isManager ? true : enabled,
          });
          // Mark form clean: the values we just persisted are now the
          // baseline. isDirty flips false and reports up so the sidebar
          // dot clears even though the modal will close on full success.
          baselinePayloadRef.current = currentPayload;
        }
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save agent.';
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  const handleDelete = useCallback(async () => {
    if (!canDelete || !agent?.id) return;
    if (!window.confirm(`Delete agent "${agent?.name}"? This cannot be undone.`)) return;
    setDeleting(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, agent.id);
      onDeleted?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to delete agent.');
    } finally {
      setDeleting(false);
    }
  }, [canDelete, agent, workspaceId, onDeleted]);

  if (loading) {
    return <div className={styles.sectionRoot}>Loading…</div>;
  }

  return (
    <div className={styles.sectionRoot}>
      <h3 className={styles.sectionTitle}>
        {isCreate ? 'Set up the main agent' : (isManager ? 'Main agent' : (agent?.name || 'Agent'))}
      </h3>
      <p className={styles.sectionDescription}>
        {/* Creating an agent here means configuring this workspace's own Main.
            Teammates come from the shared library and are added under Team. */}
        {isCreate
          ? "This workspace has no main agent yet. It runs whenever you send a message or the schedule fires; teammates are added from the shared agent library under Team."
          : isManager
            ? "This workspace's main agent. It's always present and runs whenever you send a message or the schedule fires."
            : 'Teammate — invoked by the main agent via delegation.'}
      </p>

      {/* Name (hidden for manager — its name is "Main" by convention) */}
      {!isManager && (
        <div className={styles.field}>
          <label className={styles.label} htmlFor="agent-name">
            Name <span className={styles.required}>*</span>
          </label>
          <input
            id="agent-name"
            type="text"
            className={styles.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={busy}
            maxLength={100}
          />
        </div>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor="agent-description">Description</label>
        <textarea
          id="agent-description"
          className={styles.textarea}
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          disabled={busy}
          rows={5}
          placeholder={isManager
            ? 'Instructions for how the main agent behaves in this workspace…'
            : 'What this sub-agent does and when the main agent should delegate to it…'}
        />
        <span className={styles.hint}>
          Markdown supported. Appended to the agent&apos;s system prompt at runtime.
        </span>
      </div>

      {/* Skills */}
      <div className={styles.field}>
        <label className={styles.label} id="agent-skills-label">Skills</label>
        <div role="group" aria-labelledby="agent-skills-label">
          <SkillPicker
            skills={deps?.skills || []}
            selectedIds={selectedSkillIds}
            onChange={setSelectedSkillIds}
            disabled={busy}
          />
        </div>
      </div>

      {/* MCP Servers */}
      <div className={styles.field}>
        <label className={styles.label}>MCP servers</label>
        {(deps?.mcpServers || []).length === 0 ? (
          <span className={styles.hint}>No MCP servers configured.</span>
        ) : (
          <div className={styles.checkboxGroup}>
            {(deps?.mcpServers || []).map((server) => {
              const checked = selectedMcpServerIds.includes(server.id);
              return (
                <label key={server.id} className={styles.checkboxOption}>
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => {
                      if (e.target.checked) {
                        setSelectedMcpServerIds((s) => [...s, server.id]);
                      } else {
                        setSelectedMcpServerIds((s) => s.filter((id) => id !== server.id));
                      }
                    }}
                    disabled={busy}
                  />
                  <span>{server.name}</span>
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* Provider connections */}
      <div className={styles.field}>
        <label className={styles.label}>
          Provider connections <span className={styles.required}>*</span>
        </label>
        {providerConnectionIds.length > 0 && (
          <div className={styles.chipList}>
            {providerConnectionIds.map((id) => {
              const conn = enabledProviderConnections.find((c) => c.id === id);
              return (
                <span key={id} className={styles.chip}>
                  {conn?.name || id}
                  <button
                    type="button"
                    className={styles.chipRemove}
                    onClick={() => setProviderConnectionIds((s) => s.filter((x) => x !== id))}
                    disabled={busy}
                    aria-label={`Remove ${conn?.name || id}`}
                  >
                    ×
                  </button>
                </span>
              );
            })}
          </div>
        )}
        {availableProviderConnections.length > 0 && (
          <div className={styles.listInputRow}>
            <select
              className={styles.select}
              value={providerConnectionDraft}
              onChange={(e) => setProviderConnectionDraft(e.target.value)}
              disabled={busy}
            >
              {availableProviderConnections.map((c) => (
                <option key={c.id} value={c.id}>{c.name}</option>
              ))}
            </select>
            <button
              type="button"
              className={styles.addButton}
              onClick={() => {
                if (!providerConnectionDraft) return;
                setProviderConnectionIds((s) => addUniqueItem(s, providerConnectionDraft));
              }}
              disabled={!providerConnectionDraft || saving}
            >
              Add
            </button>
          </div>
        )}
      </div>

      {/* Local capabilities */}
      <h4 className={styles.sectionTitle} style={{ marginTop: 24 }}>Local capabilities</h4>
      <p className={styles.sectionDescription}>
        What this agent can do on your machine. Every new agent ships with <code>$HOME</code> read-only — remove it below if you want this agent fully isolated.
      </p>

      <div className={styles.field}>
        <label className={styles.label}>Additional path grants</label>
        {extraPathGrants.length > 0 && (
          <div className={styles.grantList}>
            {extraPathGrants.map((grant) => (
              <div key={grant.path} className={styles.grantItem}>
                <span className={styles.grantPath}>{grant.path}</span>
                <span className={styles.grantAccess}>{grant.access === 'read_write' ? 'RW' : 'RO'}</span>
                <span className={styles.grantOrigin} title={grant.origin?.reason || undefined}>
                  {grantOriginLabel(grant.origin)}
                </span>
                <button
                  type="button"
                  className={styles.chipRemove}
                  onClick={() => setExtraPathGrants((s) => s.filter((g) => g.path !== grant.path))}
                  disabled={busy}
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
            value={extraPathDraft}
            onChange={(e) => setExtraPathDraft(e.target.value)}
            placeholder="/home/user/project"
            disabled={busy}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); handleAddPathGrant(); } }}
          />
          <select
            className={styles.select}
            value={extraPathAccess}
            onChange={(e) => setExtraPathAccess(e.target.value)}
            disabled={busy}
          >
            <option value="read_only">Read only</option>
            <option value="read_write">Read &amp; write</option>
          </select>
          <button
            type="button"
            className={styles.addButton}
            onClick={handleAddPathGrant}
            disabled={!extraPathDraft.trim() || saving}
          >
            Add
          </button>
        </div>
      </div>

      <div className={styles.field}>
        <label className={styles.label} htmlFor="shell-mode">Shell access</label>
        <select
          id="shell-mode"
          className={styles.select}
          value={shellMode}
          onChange={(e) => setShellMode(e.target.value)}
          disabled={busy}
        >
          <option value="off">Off</option>
          <option value="restricted">Restricted (allow/block lists)</option>
          <option value="full">Full</option>
        </select>
        <span className={styles.hint}>
          Agents use the shell for local file operations, so Off leaves the agent with no way to read
          or write files — charts and conversation history still work. Under Restricted, a command
          outside the allowed list stops for your approval, so an allowed list with nothing that
          writes leaves the agent able to read but not save.
        </span>
      </div>

      {shellMode === 'restricted' && (
        <>
          <div className={styles.field}>
            <label className={styles.label}>Allowed command prefixes</label>
            {allowedCommands.length > 0 && (
              <div className={styles.commandList}>
                {allowedCommands.map((cmd) => (
                  <div key={cmd} className={styles.commandItem}>
                    <code className={styles.commandPrefix}>{cmd}</code>
                    <button
                      type="button"
                      className={styles.chipRemove}
                      onClick={() => setAllowedCommands((s) => s.filter((c) => c !== cmd))}
                      disabled={busy}
                      aria-label={`Remove ${cmd}`}
                    >
                      ×
                    </button>
                  </div>
                ))}
              </div>
            )}
            {allowedCommands.length === 0 && (
              <span className={styles.hint}>No allowed prefixes configured.</span>
            )}
            <div className={styles.listInputRow}>
              <input
                type="text"
                className={styles.input}
                value={allowedCommandDraft}
                onChange={(e) => setAllowedCommandDraft(e.target.value)}
                placeholder="git status"
                disabled={busy}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    handleAddAllowedCommand();
                  }
                }}
              />
              <button
                type="button"
                className={styles.addButton}
                onClick={handleAddAllowedCommand}
                disabled={!allowedCommandDraft.trim() || busy}
              >
                Add
              </button>
            </div>
          </div>

          <div className={styles.field}>
            <label className={styles.label}>Blocked command prefixes</label>
            {blockedCommands.length > 0 && (
              <div className={styles.commandList}>
                {blockedCommands.map((cmd) => (
                  <div key={cmd} className={styles.commandItem}>
                    <code className={styles.commandPrefix}>{cmd}</code>
                    <button
                      type="button"
                      className={styles.chipRemove}
                      onClick={() => setBlockedCommands((s) => s.filter((c) => c !== cmd))}
                      disabled={busy}
                      aria-label={`Remove ${cmd}`}
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
                value={blockedCommandDraft}
                onChange={(e) => setBlockedCommandDraft(e.target.value)}
                placeholder="rm -rf"
                disabled={busy}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    handleAddBlockedCommand();
                  }
                }}
              />
              <button
                type="button"
                className={styles.addButton}
                onClick={handleAddBlockedCommand}
                disabled={!blockedCommandDraft.trim() || busy}
              >
                Add
              </button>
            </div>
          </div>
        </>
      )}

      <div className={styles.field}>
        <label className={styles.toggleRow}>
          <span className={styles.toggleLabel}>Web access (fetch, search)</span>
          <span className={`${styles.toggle} ${webEnabled ? styles.toggleOn : ''}`}>
            <input
              type="checkbox"
              className={styles.toggleInput}
              checked={webEnabled}
              onChange={(e) => setWebEnabled(e.target.checked)}
              disabled={busy}
            />
            <span className={styles.toggleTrack}>
              <span className={styles.toggleThumb} />
            </span>
          </span>
        </label>
      </div>

      <div className={styles.field}>
        <label className={styles.toggleRow}>
          <span className={styles.toggleLabel}>Allow session D-Bus (libsecret / keyring)</span>
          <span className={`${styles.toggle} ${sessionBusAllowed ? styles.toggleOn : ''}`}>
            <input
              type="checkbox"
              className={styles.toggleInput}
              checked={sessionBusAllowed}
              onChange={(e) => setSessionBusAllowed(e.target.checked)}
              disabled={busy}
            />
            <span className={styles.toggleTrack}>
              <span className={styles.toggleThumb} />
            </span>
          </span>
        </label>
      </div>

      {/* Enabled toggle (sub-agents only — manager is always enabled) */}
      {!isManager && !isCreate && (
        <div className={styles.field}>
          <label className={styles.toggleRow}>
            <span className={styles.toggleLabel}>Enabled</span>
            <span className={`${styles.toggle} ${enabled ? styles.toggleOn : ''}`}>
              <input
                type="checkbox"
                className={styles.toggleInput}
                checked={enabled}
                onChange={(e) => setEnabled(e.target.checked)}
                disabled={busy}
              />
              <span className={styles.toggleTrack}>
                <span className={styles.toggleThumb} />
              </span>
            </span>
          </label>
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      {canDelete && (
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.dangerButton}
            onClick={handleDelete}
            disabled={busy}
          >
            {deleting ? 'Deleting…' : 'Delete agent'}
          </button>
        </div>
      )}
    </div>
  );
};

// Initial draft used by the "Add agent" form. The backend-provided
// `defaultExecution` (carries the `$HOME` RO grant) is preferred so the
// user sees the granted paths up front. Falls back to the local empty
// execution if the fetch failed — better to show an empty list than to
// block the whole create flow.
const blankAgentDraft = (defaultExecutionFromBackend?: Partial<ExecutionConfig>): AgentDetail => ({
  id: '',
  isDefault: false,
  name: '',
  description: '',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: [],
  execution: defaultExecutionFromBackend || defaultExecution(),
  enabled: true,
});

export default AgentBehaviorForm;
