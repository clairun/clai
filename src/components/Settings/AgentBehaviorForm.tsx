/**
 * AgentBehaviorForm
 *
 * The one form that edits what an agent *is*: instructions, the model it
 * runs on, MCP servers, skills and local access. Two hosts render it — the
 * workspace settings window (this workspace's Main) and the agent library
 * (a shared definition, saved through `saveBehavior`). Both drive it through
 * `SectionHandle`.
 *
 * Laid out as an overview that fits one screen, with two drill-in views for
 * what does not: one skill source, and local access. `back()` on the handle
 * leaves a drill-in view.
 */

import React, { useCallback, useEffect, useId, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  workspaceCreateAgent,
  workspaceDeleteAgent,
  workspaceGetAgent,
  workspaceUpdateAgent,
} from '../../api/client';
import type {
  ExecutionCapabilityConfig,
  FilesystemPathGrant,
  ProviderConnection,
} from '../../generated/bindings';
import { openGlobalSettings } from '../../utils/globalSettings';
import { useReportDirty, type SectionHandle } from './sectionHandle';
import McpServerSelect from './McpServerSelect';
import { NoSkills, SkillSearch, SkillSources, SkillSourceView, groupSkills } from './SkillPicker';
import { AccessView, accessSummary, type AccessDraft } from './AgentAccess';
import { Switch } from './ListEditors';
import form from './SettingsForm.module.css';
import styles from './AgentBehaviorForm.module.css';

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
  execution: ExecutionCapabilityConfig;
  enabled: boolean;
}

// The agent the form edits: a `WorkspaceAgentDetail` from `workspaceGetAgent`,
// or a library definition the host passes as `initialAgent`.
export interface AgentDetail {
  id: string;
  name?: string;
  description?: string;
  isDefault?: boolean;
  enabled?: boolean;
  selectedSkillIds?: string[];
  selectedMcpServerIds?: string[];
  providerConnectionIds?: string[];
  execution?: Partial<ExecutionCapabilityConfig>;
}

export interface AgentFormDeps {
  mcpServers: NamedRef[];
  skills: SkillRef[];
  providerConnections: ProviderConnection[];
  defaultExecution: Partial<ExecutionCapabilityConfig> | null | undefined;
}

type View = { kind: 'overview' } | { kind: 'source'; key: string } | { kind: 'access' };

// ── Helpers ─────────────────────────────────────────────────────────

const DEFAULT_BLOCKED = ['rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot'];

const clean = (items: string[] = []): string[] => items.map((item) => item.trim()).filter(Boolean);

const accessFrom = (execution: Partial<ExecutionCapabilityConfig> = {}): AccessDraft => ({
  grants: (execution.filesystem?.extraPaths || [])
    .map((grant: FilesystemPathGrant) => ({
      path: grant.path?.trim() || '',
      access: grant.access || 'read_only',
      origin: grant.origin || null,
    }))
    .filter((grant) => grant.path),
  shellMode: execution.shell?.mode || 'off',
  allowed: clean(execution.shell?.allowedCommandPrefixes || []),
  blocked: clean(execution.shell?.blockedCommandPrefixes || DEFAULT_BLOCKED),
  web: execution.web?.enabled || false,
  sessionBus: (execution.sandbox?.sessionBus || 'allow') === 'allow',
});

const executionFrom = (access: AccessDraft): ExecutionCapabilityConfig => ({
  sandbox: { network: 'enabled', sessionBus: access.sessionBus ? 'allow' : 'deny' },
  filesystem: { extraPaths: access.grants },
  shell: {
    mode: access.shellMode,
    allowedCommandPrefixes: access.allowed,
    blockedCommandPrefixes: access.blocked,
  },
  web: { enabled: access.web },
});

interface Draft {
  name: string;
  description: string;
  skillIds: string[];
  mcpIds: string[];
  connectionIds: string[];
  access: AccessDraft;
  enabled: boolean;
}

const draftFrom = (agent: AgentDetail): Draft => ({
  name: agent.name || '',
  description: agent.description || '',
  skillIds: agent.selectedSkillIds || [],
  mcpIds: agent.selectedMcpServerIds || [],
  connectionIds: agent.providerConnectionIds || [],
  access: accessFrom(agent.execution),
  enabled: agent.enabled !== false,
});

// Canonical form of the editable fields; dirty = differs from the baseline.
const serialize = (draft: Draft): string => JSON.stringify({
  ...draft,
  name: draft.name.trim(),
  description: draft.description.trim(),
});

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const connectionLabel = (connection: ProviderConnection): string =>
  connection.modelId ? `${connection.name} — ${connection.modelId}` : connection.name;

// Initial draft for a new agent. The backend's default execution carries the
// `$HOME` read-only grant, so the user sees it before Save rather than having
// it injected after; a failed fetch falls back to the local defaults.
const blankAgent = (execution?: Partial<ExecutionCapabilityConfig>): AgentDetail => ({
  id: '',
  isDefault: false,
  name: '',
  description: '',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: [],
  execution: execution || { shell: { mode: 'off', allowedCommandPrefixes: [], blockedCommandPrefixes: DEFAULT_BLOCKED } },
  enabled: true,
});

// ── Form ────────────────────────────────────────────────────────────

export const AgentBehaviorForm = ({
  ref,
  workspaceId,
  agentId,             // string for edit; null for create
  initialAgent,
  saveBehavior,
  deps,
  saving,              // host save in flight — disables inputs
  onDirtyChange,
  onDeleted,
}: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  agentId: string | null;
  initialAgent?: AgentDetail;
  saveBehavior?: (payload: AgentBehaviorPayload) => Promise<void>;
  deps: AgentFormDeps;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
  onDeleted?: () => void;
}) => {
  const isCreate = !agentId;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const busy = saving || deleting;

  const [agent, setAgent] = useState<AgentDetail | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  // State, not a ref: a new baseline after a save must re-render so the
  // dirty flag flips and is reported, whether or not the host re-renders.
  const [baseline, setBaseline] = useState<string | null>(null);
  const [view, setView] = useState<View>({ kind: 'overview' });
  const [skillQuery, setSkillQuery] = useState('');

  const isManager = agent?.isDefault === true;
  const canDelete = !saveBehavior && !isCreate && !isManager;

  const viewHeading = useRef<HTMLHeadingElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  // The overview control that opened the current view; focus returns to it.
  const opener = useRef<string | null>(null);
  const lastFetchedId = useRef<string | null>(null);
  const ids = useId();

  useEffect(() => {
    if (!agentId || lastFetchedId.current === agentId) return undefined;
    let cancelled = false;
    setLoading(true);
    setError(null);
    (initialAgent ? Promise.resolve(initialAgent) : workspaceGetAgent(workspaceId, agentId))
      .then((detail) => {
        if (cancelled) return;
        if (detail === null) {
          setError('Agent not found.');
          return;
        }
        lastFetchedId.current = agentId;
        setAgent(detail);
      })
      .catch((err) => {
        if (!cancelled) setError(errText(err, 'Failed to load agent.'));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [workspaceId, agentId, initialAgent]);

  // A create waits for the backend defaults (or their failure) before it
  // shows anything, so the default grants are on screen from the start.
  useEffect(() => {
    if (!isCreate || agent || deps.defaultExecution === undefined) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- bootstraps the create draft once the defaults fetch has settled.
    setAgent(blankAgent(deps.defaultExecution || undefined));
    setLoading(false);
  }, [isCreate, agent, deps.defaultExecution]);

  useEffect(() => {
    if (!agent) return;
    const next = draftFrom(agent);
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the draft and its baseline are rebuilt together whenever the source agent changes.
    setDraft(next);
    setBaseline(serialize(next));
  }, [agent]);

  const isDirty = draft !== null && baseline !== null && serialize(draft) !== baseline;
  useReportDirty(isDirty, onDirtyChange);

  const patch = useCallback((change: Partial<Draft>) => {
    setDraft((current) => (current ? { ...current, ...change } : current));
  }, []);

  const groups = useMemo(() => groupSkills(deps.skills || []), [deps.skills]);
  const enabledConnections = useMemo(
    () => (deps.providerConnections || []).filter((c) => c.enabled),
    [deps.providerConnections]
  );

  const openView = useCallback((next: View, from: string) => {
    opener.current = from;
    setView(next);
  }, []);

  const backToOverview = useCallback(() => setView({ kind: 'overview' }), []);

  // Entering a view focuses its heading; returning focuses what opened it.
  useLayoutEffect(() => {
    if (view.kind !== 'overview') {
      viewHeading.current?.focus();
      return;
    }
    const from = opener.current;
    if (!from) return;
    opener.current = null;
    rootRef.current?.querySelector<HTMLElement>(`[data-opener="${from}"], [data-source="${from}"]`)?.focus();
  }, [view]);

  const validate = useCallback(() => {
    if (!draft) return { ok: false, error: 'Nothing to save yet.' };
    const name = draft.name.trim();
    if (!isManager && !name) return { ok: false, error: 'Agent name is required.' };
    if (name.length > 100) return { ok: false, error: 'Name must be 100 characters or less.' };
    if (draft.connectionIds.length === 0) return { ok: false, error: 'Choose a model for this agent to run on.' };
    return { ok: true };
  }, [draft, isManager]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const verdict = validate();
      setError(verdict.ok ? null : (verdict.error ?? null));
      return verdict;
    },
    submit: async () => {
      const verdict = validate();
      if (!verdict.ok || !draft) {
        setError(verdict.error ?? null);
        return verdict;
      }
      const payload = {
        name: draft.name.trim(),
        description: draft.description.trim(),
        selectedSkillIds: draft.skillIds,
        selectedMcpServerIds: draft.mcpIds,
        providerConnectionIds: draft.connectionIds,
        execution: executionFrom(draft.access),
      };
      try {
        if (saveBehavior) {
          await saveBehavior({ workspaceId, ...payload, enabled: draft.enabled });
        } else if (isCreate) {
          const created = await workspaceCreateAgent({ workspaceId, ...payload, enabled: draft.enabled });
          setBaseline(serialize(draft));
          // The host moves to the new agent's own page.
          return { ok: true, createdId: created.id };
        } else {
          await workspaceUpdateAgent({
            workspaceId,
            agentId: agent?.id || '',
            ...payload,
            // The Main keeps its stored name and is always on. The baseline
            // below is the form's own serialisation, so this never dirties it.
            name: isManager ? (agent?.name || 'Manager') : payload.name,
            enabled: isManager ? true : draft.enabled,
          });
        }
        setBaseline(serialize(draft));
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = errText(err, 'Failed to save agent.');
        setError(message);
        return { ok: false, error: message };
      }
    },
    back: () => {
      if (view.kind === 'overview') return false;
      backToOverview();
      return true;
    },
  }));

  const handleDelete = useCallback(async () => {
    if (!canDelete || !agent?.id) return;
    if (!window.confirm(`Delete agent "${agent.name}"? This cannot be undone.`)) return;
    setDeleting(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, agent.id);
      onDeleted?.();
    } catch (err) {
      setError(errText(err, 'Failed to delete agent.'));
    } finally {
      setDeleting(false);
    }
  }, [canDelete, agent, workspaceId, onDeleted]);

  // Hosts load every dep at once, so settled defaults mean the connection,
  // skill and server lists are in too: until then the overview would warn
  // about a missing connection and empty lists that are only loading.
  if (loading || deps.defaultExecution === undefined || (agent && !draft)) {
    return (
      <div className={form.page} aria-busy="true" aria-label="Loading agent">
        <div className={form.skeleton}><span /><span /><span /></div>
      </div>
    );
  }

  // Nothing to edit if the fetch failed: no fields, no Delete to refuse.
  if (!agent || !draft) {
    return (
      <div className={form.page}>
        <p className={form.errorBanner} role="alert">{error || 'Failed to load agent.'}</p>
      </div>
    );
  }

  const context = isManager ? 'Main' : (draft.name.trim() || 'Agent');
  const errorBanner = error && <p className={form.errorBanner} role="alert">{error}</p>;

  if (view.kind === 'source') {
    const group = groups.find((g) => g.key === view.key);
    if (group) {
      return (
        <div className={form.page} ref={rootRef}>
          <SkillSourceView
            group={group}
            selectedIds={draft.skillIds}
            onChange={(skillIds) => patch({ skillIds })}
            disabled={busy}
            headingRef={viewHeading}
            onBack={backToOverview}
            context={context}
          />
          {errorBanner}
        </div>
      );
    }
  }

  if (view.kind === 'access') {
    return (
      <div className={form.page} ref={rootRef}>
        <AccessView
          draft={draft.access}
          onChange={(change) => patch({ access: { ...draft.access, ...change } })}
          disabled={busy}
          headingRef={viewHeading}
          onBack={backToOverview}
          context={context}
        />
        {errorBanner}
      </div>
    );
  }

  // ── Overview ──
  const currentId = draft.connectionIds[0] ?? '';
  const current = (deps.providerConnections || []).find((c) => c.id === currentId);
  const currentUsable = !!current?.enabled;
  const extraConnections = draft.connectionIds.length - 1;
  const selectedSkillCount = draft.skillIds.filter((id) => deps.skills.some((s) => s.id === id)).length;

  return (
    <div className={form.page} ref={rootRef}>
      <div className={styles.toolbar}>
        <div className={`${form.field} ${styles.modelField}`}>
          <label className={form.label} htmlFor={`${ids}-model`}>
            Runs on <span className={form.required} aria-hidden="true">*</span>
          </label>
          {enabledConnections.length === 0 && !currentId ? (
            <p className={form.hintWarning}>
              No model connections yet.{' '}
              <button type="button" className={form.linkButton} onClick={() => openGlobalSettings({ tab: 'provider', providerAction: 'new' })}>
                Add one in Settings → Providers
              </button>
            </p>
          ) : (
            <select
              id={`${ids}-model`}
              className={`${form.select} ${!currentId || !currentUsable ? styles.selectWarn : ''}`}
              value={currentId}
              onChange={(e) => patch({ connectionIds: e.target.value ? [e.target.value] : [] })}
              disabled={busy}
              aria-describedby={`${ids}-model-hint`}
            >
              {!currentId && <option value="" disabled>Choose a model…</option>}
              {currentId && !currentUsable && (
                <option value={currentId} disabled>
                  {current ? `${connectionLabel(current)} (disabled)` : 'Unavailable connection'}
                </option>
              )}
              {enabledConnections.map((c) => (
                <option key={c.id} value={c.id}>{connectionLabel(c)}</option>
              ))}
            </select>
          )}
          <p className={form.hint} id={`${ids}-model-hint`}>
            {!currentId
              ? 'Pick the connection this agent runs on.'
              : !currentUsable
                ? 'This connection is gone or turned off. Pick another one.'
                : extraConnections > 0
                  ? `${extraConnections} more saved connection${extraConnections === 1 ? ' is' : 's are'} never used; picking another replaces them all.`
                  : 'One connection, no automatic fallback.'}
          </p>
        </div>

        <div className={`${form.field} ${styles.mcpField}`}>
          <span className={form.label} id={`${ids}-mcp`}>MCP servers</span>
          <McpServerSelect
            servers={deps.mcpServers || []}
            selectedIds={draft.mcpIds}
            onChange={(mcpIds) => patch({ mcpIds })}
            disabled={busy}
            labelId={`${ids}-mcp`}
          />
          <p className={form.hint}>Tools the agent can call.</p>
        </div>

        {(!isManager && !isCreate) && (
          <div className={styles.toolbarEnd}>
            <Switch label="Enabled" checked={draft.enabled} onChange={(enabled) => patch({ enabled })} disabled={busy} />
            {canDelete && (
              <button type="button" className={form.dangerButton} onClick={handleDelete} disabled={busy}>
                {deleting ? 'Deleting…' : 'Delete agent'}
              </button>
            )}
          </div>
        )}
      </div>

      <div className={form.columns}>
        <section className={`${form.panel} ${form.panelFill}`}>
          <header className={form.panelHeader}>
            <h3 className={form.panelTitle} id={`${ids}-instructions`}>Instructions</h3>
            <span className={form.panelMeta}>Markdown · added to the system prompt</span>
          </header>
          <div className={form.panelBody}>
            {!isManager && (
              <div className={form.field}>
                <label className={form.label} htmlFor={`${ids}-name`}>
                  Name <span className={form.required} aria-hidden="true">*</span>
                </label>
                <input
                  id={`${ids}-name`}
                  type="text"
                  className={form.input}
                  value={draft.name}
                  onChange={(e) => patch({ name: e.target.value })}
                  disabled={busy}
                  maxLength={100}
                />
              </div>
            )}
            <textarea
              className={`${form.textarea} ${form.textareaFill}`}
              aria-labelledby={`${ids}-instructions`}
              value={draft.description}
              onChange={(e) => patch({ description: e.target.value })}
              disabled={busy}
              placeholder={isManager
                ? 'How the main agent should work in this workspace…'
                : 'What this agent does, and when the main agent should hand it work…'}
            />
          </div>
        </section>

        <div className={form.column}>
          <section className={`${form.panel} ${form.panelFill}`}>
            <header className={form.panelHeader}>
              <h3 className={form.panelTitle} id={`${ids}-skills`}>Skills</h3>
              <span className={form.panelMeta}>{selectedSkillCount} of {deps.skills.length} selected</span>
              {groups.length > 0 && (
                <span className={form.panelActions}>
                  <SkillSearch value={skillQuery} onChange={setSkillQuery} disabled={busy} />
                  {selectedSkillCount > 0 && (
                    <button
                      type="button"
                      className={form.quietButton}
                      onClick={() => patch({ skillIds: draft.skillIds.filter((id) => !deps.skills.some((s) => s.id === id)) })}
                      disabled={busy}
                    >
                      Clear all
                    </button>
                  )}
                </span>
              )}
            </header>
            <div className={form.panelBody}>
              {groups.length === 0 ? (
                <NoSkills />
              ) : (
                <SkillSources
                  groups={groups}
                  query={skillQuery}
                  selectedIds={draft.skillIds}
                  onChange={(skillIds) => patch({ skillIds })}
                  onOpen={(key) => openView({ kind: 'source', key }, key)}
                  disabled={busy}
                />
              )}
            </div>
          </section>

          <section className={form.panel}>
            <header className={form.panelHeader}>
              <h3 className={form.panelTitle} id={`${ids}-access`}>Local access</h3>
              <span className={form.panelActions}>
                <button
                  type="button"
                  className={form.quietButton}
                  data-opener="access"
                  onClick={() => openView({ kind: 'access' }, 'access')}
                  aria-label="Edit local access"
                >
                  Edit ›
                </button>
              </span>
            </header>
            <div className={form.panelBody}>
              <ul className={styles.summary}>
                {accessSummary(draft.access).map((line) => <li key={line}>{line}</li>)}
              </ul>
            </div>
          </section>
        </div>
      </div>
      {errorBanner}
    </div>
  );
};
