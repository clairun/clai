/**
 * WorkspaceSettingsModal
 *
 * A settings window: a sidebar that always lists every page (General,
 * Schedule, Team; the Main and each crew member; Add to crew) and the
 * chosen page filling the rest. Pages are laid out for the window rather
 * than grown past it. Each page is a draft saved through the save bar in
 * its header, which takes no room while there is nothing to save;
 * leaving a page with unsaved changes asks first, whichever way you leave.
 */

import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import ReactDOM from 'react-dom';
import { getMcpServers, getSkills, workspaceAgentDefaultExecution } from '../../api/client';
import { assistantClient } from '../../assistant';
import { AssignmentSection, TeamPolicySection } from './WorkspaceTeamSettings';
import { AgentBehaviorForm, type AgentFormDeps } from './AgentBehaviorForm';
import { GeneralPage, SchedulePage } from './WorkspacePages';
import type { SectionHandle } from './sectionHandle';
import type { WorkspaceAgentResponse, WorkspaceDetails } from '../../generated/bindings';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';
import AgentAvatar from '../Agents/AgentAvatar';
import { identityFor } from '../Agents/agentIdentity';
import form from './SettingsForm.module.css';
import shell from './SettingsShell.module.css';
import styles from './WorkspaceSettingsModal.module.css';

type PageKind = 'general' | 'schedule' | 'team' | 'agent' | 'new-main' | 'new-agent';
interface Selection {
  kind: PageKind;
  agentId?: string | null;
}

const GENERAL: Selection = { kind: 'general' };

// Identifies a sidebar item, so focus can find it again.
const pageKey = (sel: Selection): string => (
  sel.kind === 'agent' ? `agent:${sel.agentId}` : sel.kind
);

const agentLabel = (agent: WorkspaceAgentResponse): string => (
  agent.isDefault ? 'Main' : (agent.displayName || agent.agentName || 'Untitled')
);

const scheduleSummary = (details: WorkspaceDetails | null): string => {
  if (!details?.scheduleEnabled) return 'Off';
  const kind = details.scheduleKind;
  let when = 'On';
  if (kind?.type === 'cron') {
    when = kind.expression;
  } else if (kind?.type === 'interval') {
    const mins = kind.intervalMinutes;
    when = mins % 60 === 0 ? `Every ${mins / 60} h` : `Every ${mins} min`;
  }
  return details.schedulePaused ? `${when} · paused` : when;
};

// Keep in sync with WorkspaceContextBar: the self-loading context bar
// listens for this to refetch its workspace details after a settings save.
const WORKSPACE_SETTINGS_CHANGED_EVENT = 'workspace-settings-changed';

interface WorkspaceSettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  workspaceId: string;
  details: WorkspaceDetails | null;
  /** Page to open on; General when omitted. */
  initialSelection?: Selection | null;
  onChanged?: () => void;
}

type FocusTarget = 'heading' | 'nav';

const WorkspaceSettingsModal = ({
  isOpen,
  onClose,
  workspaceId,
  details,
  initialSelection,
  onChanged,
}: WorkspaceSettingsModalProps) => {
  // Structural compare: callers pass inline literals with a fresh identity
  // every render, which must not snap the window back to its first page.
  const initialKey = JSON.stringify(initialSelection ?? null);
  const initialPage = useMemo<Selection>(() => JSON.parse(initialKey) ?? GENERAL, [initialKey]);

  const [page, setPage] = useState<Selection>(initialPage);
  // Bumped to remount the page: that is how Discard drops its draft.
  const [pageVersion, setPageVersion] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Set while the unsaved-changes question is up: where to go once answered.
  const [pendingLeave, setPendingLeave] = useState<(() => void) | null>(null);
  const [deps, setDeps] = useState<AgentFormDeps>({
    mcpServers: [],
    skills: [],
    providerConnections: [],
    // `undefined` while loading, `null` if the fetch failed. The create form
    // waits for either, so the default grants show before Save, not after.
    defaultExecution: undefined,
  });

  const pageRef = useRef<SectionHandle>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const navRef = useRef<HTMLElement>(null);
  // Where focus goes once the next page has rendered.
  const focusNext = useRef<FocusTarget | null>(null);
  // What had focus when the leave question opened; Stay hands it back.
  const leaveFocus = useRef<HTMLElement | null>(null);

  const showPage = useCallback((next: Selection, focus: FocusTarget | null) => {
    focusNext.current = focus;
    setPage(next);
    setDirty(false);
    setSaveError(null);
    setPendingLeave(null);
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- resets navigation and save state when the window opens or the caller asks for a different page.
    showPage(initialPage, 'nav');
    setSaving(false);
  }, [isOpen, initialPage, showPage]);

  const focusNav = useCallback((key: string) => {
    const nav = navRef.current;
    (nav?.querySelector<HTMLElement>(`[data-nav="${key}"]`) ?? nav?.querySelector<HTMLElement>('[data-nav]'))?.focus();
  }, []);

  useEffect(() => {
    if (!isOpen || !focusNext.current) return;
    const target = focusNext.current;
    focusNext.current = null;
    if (target === 'heading') headingRef.current?.focus();
    else focusNav(pageKey(page));
  }, [isOpen, page, pageVersion, focusNav]);

  useEffect(() => {
    if (!isOpen) return undefined;
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
        mcpServers: servers.status === 'fulfilled' ? (servers.value || []) : [],
        skills: skills.status === 'fulfilled' ? (skills.value || []) : [],
        providerConnections: connections.status === 'fulfilled' ? (connections.value || []) : [],
        defaultExecution: defaults.status === 'fulfilled' ? (defaults.value || null) : null,
      });
    })();
    return () => { cancelled = true; };
  }, [isOpen]);

  // Saves the visible page and stays on it. Resolves true on success.
  const savePage = useCallback(async () => {
    const handle = pageRef.current;
    if (!handle || saving) return false;
    const checked = handle.validate();
    if (!checked.ok) {
      setSaveError(checked.error ?? 'Validation failed.');
      return false;
    }
    setSaveError(null);
    setSaving(true);
    let createdId: string | undefined;
    try {
      const result = await handle.submit();
      if (!result.ok) {
        setSaveError(result.error || 'Save failed.');
        return false;
      }
      createdId = result.createdId;
    } catch (err) {
      setSaveError(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      setSaving(false);
    }
    // A create continues on the new record's own page before the refetch,
    // so a second Save updates it instead of creating another. Otherwise the
    // save bar is about to unmount with focus in it: hand focus to the title.
    if (createdId) {
      showPage({ kind: 'agent', agentId: createdId }, 'heading');
    } else {
      headingRef.current?.focus();
    }
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    await Promise.resolve(onChanged?.());
    return true;
  }, [saving, onChanged, showPage]);

  const discardPage = useCallback(() => {
    focusNext.current = 'heading';
    setPageVersion((v) => v + 1);
    setDirty(false);
    setSaveError(null);
  }, []);

  const leave = useCallback((go: () => void) => {
    if (saving) return;
    if (!dirty) {
      go();
      return;
    }
    leaveFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setPendingLeave(() => go);
  }, [saving, dirty]);

  const stay = useCallback(() => {
    setPendingLeave(null);
    const back = leaveFocus.current;
    (back?.isConnected && back !== document.body ? back : headingRef.current)?.focus();
  }, []);

  const navigate = useCallback((next: Selection) => {
    if (pageKey(next) === pageKey(page)) return;
    leave(() => showPage(next, 'nav'));
  }, [leave, page, showPage]);

  const handleClose = useCallback(() => leave(onClose), [leave, onClose]);

  const answerLeave = useCallback(async (answer: 'stay' | 'discard' | 'save') => {
    const go = pendingLeave;
    if (!go || answer === 'stay') {
      stay();
      return;
    }
    setPendingLeave(null);
    if (answer === 'save' && !(await savePage())) {
      stay();
      return;
    }
    go();
  }, [pendingLeave, savePage, stay]);

  // Escape, innermost first: answer Stay to an open question, let a search
  // field with text clear itself, leave a drill-in view, then close. It
  // only reaches this window while nothing is stacked over it (the global
  // Settings modal and popovers take it first through the overlay stack).
  const handleEscape = useCallback(() => {
    if (pendingLeave) {
      stay();
      return;
    }
    const focused = document.activeElement;
    if (focused instanceof HTMLInputElement && focused.type === 'search' && focused.value) return;
    if (!saving && pageRef.current?.back?.()) return;
    handleClose();
  }, [pendingLeave, saving, handleClose, stay]);

  useOverlayLayer(isOpen, handleEscape);

  const handleOverlay = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) handleClose();
  }, [handleClose]);

  const agents = useMemo(() => details?.assignedAgents || [], [details?.assignedAgents]);
  const sortedAgents = useMemo(() => (
    [...agents].sort((a, b) => (a.isDefault === b.isDefault ? 0 : a.isDefault ? -1 : 1))
  ), [agents]);

  // The page's record is gone, so there is nothing to keep or ask about.
  const handleAgentRemoved = useCallback(() => {
    showPage(GENERAL, 'nav');
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    onChanged?.();
  }, [showPage, onChanged]);

  if (!isOpen) return null;

  const modelName = (agent: WorkspaceAgentResponse): string => {
    const connection = deps.providerConnections.find((c) => c.id === agent.providerConnectionIds[0]);
    if (!connection) return agent.enabled ? '' : 'Disabled';
    return agent.enabled ? connection.name : `${connection.name} · disabled`;
  };

  const pageTitle = (sel: Selection): string => {
    switch (sel.kind) {
      case 'general': return 'General';
      case 'schedule': return 'Schedule';
      case 'team': return 'Team';
      case 'new-main': return 'Set up the main agent';
      case 'new-agent': return 'Add to crew';
      case 'agent': {
        const agent = agents.find((a) => a.id === sel.agentId);
        return agent ? agentLabel(agent) : 'Main';
      }
    }
  };

  const pageSubtitle = (sel: Selection): string | null => {
    switch (sel.kind) {
      case 'general': return 'What this workspace is called and where it lives.';
      case 'schedule': return 'Run the main agent on its own, on a timer or a cron schedule.';
      case 'team': return 'What every agent in this workspace shares.';
      case 'new-main': return 'This workspace has no main agent yet. It answers your messages and runs on the schedule.';
      case 'new-agent': return 'Crew members come from the shared agent library. The main agent delegates work to them.';
      case 'agent': {
        const agent = agents.find((a) => a.id === sel.agentId);
        if (agent && !agent.isDefault) return 'Crew member. What it can do comes from its shared definition; what it knows about this workspace is set here.';
        return 'Answers your messages and runs on the schedule. Changes apply from its next turn.';
      }
    }
  };

  const renderPage = (sel: Selection) => {
    const pageProps = { ref: pageRef, workspaceId, saving, onDirtyChange: setDirty };
    switch (sel.kind) {
      case 'general':
        return <GeneralPage {...pageProps} details={details} />;
      case 'schedule':
        return <SchedulePage {...pageProps} details={details} onDetailsRefresh={onChanged} />;
      case 'team':
        return <TeamPolicySection {...pageProps} />;
      case 'new-agent':
        return <AssignmentSection workspaceId={workspaceId} onChanged={onChanged} />;
      case 'new-main':
      case 'agent': {
        // Only the Main is edited here. A crew member's behaviour belongs to
        // its shared definition, so its page is the local-overlay editor. An
        // id missing from the roster is a Main just created whose refetch has
        // not landed: crew members are only opened from their own item.
        const isCrew = sel.kind === 'agent' && agents.find((a) => a.id === sel.agentId)?.isDefault === false;
        if (isCrew) {
          return (
            <AssignmentSection
              {...pageProps}
              agentId={sel.agentId ?? undefined}
              onChanged={onChanged}
              onUnassigned={handleAgentRemoved}
            />
          );
        }
        return (
          <AgentBehaviorForm
            {...pageProps}
            agentId={sel.kind === 'agent' ? (sel.agentId ?? null) : null}
            deps={deps}
            onDeleted={handleAgentRemoved}
          />
        );
      }
    }
  };

  const currentKey = pageKey(page);
  const crewCount = agents.filter((a) => !a.isDefault).length;
  const workspaceTitle = details?.title || 'Workspace';
  const subtitle = pageSubtitle(page);

  const navItem = (sel: Selection, label: string, summary: string, icon?: React.ReactNode) => {
    const key = pageKey(sel);
    return (
      <li key={key}>
        <button
          type="button"
          data-nav={key}
          className={shell.navItem}
          aria-current={key === currentKey ? 'page' : undefined}
          onClick={() => navigate(sel)}
        >
          {icon && <span className={shell.navIcon}>{icon}</span>}
          <span className={shell.navText}>
            <span className={shell.navLabel}>{label}</span>
            {summary && <span className={shell.navSummary}>{summary}</span>}
          </span>
        </button>
      </li>
    );
  };

  return ReactDOM.createPortal(
    <div className={shell.overlay} onClick={handleOverlay}>
      <div
        className={shell.window}
        role="dialog"
        aria-modal="true"
        aria-label={`${workspaceTitle} settings`}
        onClick={(e) => e.stopPropagation()}
      >
        <nav className={shell.sidebar} aria-label="Settings pages" ref={navRef} onKeyDown={moveNavFocus}>
          <div className={shell.sidebarTitle}>
            <span className={shell.sidebarHeading} title={workspaceTitle}>{workspaceTitle}</span>
            <span className={shell.sidebarCaption}>Workspace settings</span>
          </div>

          <div className={shell.navGroup} id="nav-workspace">Workspace</div>
          <ul className={shell.navList} aria-labelledby="nav-workspace">
            {navItem(GENERAL, 'General', '')}
            {navItem({ kind: 'schedule' }, 'Schedule', scheduleSummary(details))}
            {navItem({ kind: 'team' }, 'Team', crewCount ? `${crewCount} crew` : 'No crew yet')}
          </ul>

          <div className={shell.navGroup} id="nav-agents">Agents</div>
          <ul className={shell.navList} aria-labelledby="nav-agents">
            {!details?.defaultWorkspaceAgentId
              && navItem({ kind: 'new-main' }, 'Main', 'Not set up yet', <span className={styles.navPlaceholder} aria-hidden="true" />)}
            {sortedAgents.map((agent) => navItem(
              { kind: 'agent', agentId: agent.id },
              agentLabel(agent),
              modelName(agent),
              <AgentAvatar identity={identityFor(agent)} size={22} activity={agent.enabled ? 'none' : 'disabled'} />,
            ))}
            {navItem({ kind: 'new-agent' }, 'Add to crew', '', <span className={styles.navAdd} aria-hidden="true">+</span>)}
          </ul>
        </nav>

        <div className={shell.main}>
          <header className={shell.header}>
            <div className={shell.headerText}>
              <h2 className={shell.title} ref={headingRef} tabIndex={-1}>{pageTitle(page)}</h2>
              {subtitle && <p className={shell.subtitle} title={subtitle}>{subtitle}</p>}
            </div>
            {(dirty || pendingLeave || saveError) && (
              <SaveBar
                saving={saving}
                error={saveError}
                leaving={!!pendingLeave}
                onAnswer={answerLeave}
                onDiscard={discardPage}
                onSave={savePage}
              />
            )}
            <button type="button" className={shell.closeButton} onClick={handleClose} aria-label="Close settings">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </header>

          <div className={shell.body} key={`${currentKey}:${pageVersion}`}>
            {renderPage(page)}
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
};

// ↑/↓ between sidebar items; Tab still works as usual.
const moveNavFocus = (e: React.KeyboardEvent<HTMLElement>) => {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('[data-nav]'));
  const at = items.indexOf(document.activeElement as HTMLElement);
  if (at < 0) return;
  e.preventDefault();
  const step = e.key === 'ArrowDown' ? 1 : items.length - 1;
  items[(at + step) % items.length]?.focus();
};

// Shown only while there is something to say: unsaved changes, a failed
// save, or the question asked when leaving a page with unsaved changes.
const SaveBar = ({ saving, error, leaving, onAnswer, onDiscard, onSave }: {
  saving: boolean;
  error: string | null;
  leaving: boolean;
  onAnswer: (answer: 'stay' | 'discard' | 'save') => void;
  onDiscard: () => void;
  onSave: () => void;
}) => (
  <div className={`${styles.saveBar} ${leaving ? styles.saveBarAsking : ''}`} role="region" aria-label="Unsaved changes">
    {error ? (
      <span className={styles.saveBarError} role="alert" title={error}>{error}</span>
    ) : (
      <span className={styles.saveBarText} role={leaving ? 'alert' : undefined}>
        <span className={styles.saveBarDot} aria-hidden="true" />
        <span className={styles.saveBarLabel}>{leaving ? 'Save your changes before leaving?' : 'Unsaved changes'}</span>
      </span>
    )}
    {/* Keyed so the question's buttons are new elements: reusing the
        Discard button's node for Stay would skip its autoFocus. */}
    {leaving ? (
      <React.Fragment key="ask">
        {/* Focused so Enter is the harmless answer. */}
        <button type="button" className={form.secondaryButton} onClick={() => onAnswer('stay')} autoFocus>
          Stay
        </button>
        <button type="button" className={form.secondaryButton} onClick={() => onAnswer('discard')}>
          Discard
        </button>
        <button type="button" className={form.primaryButton} onClick={() => onAnswer('save')} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </React.Fragment>
    ) : (
      <React.Fragment key="edit">
        <button type="button" className={form.secondaryButton} onClick={onDiscard} disabled={saving}>
          Discard
        </button>
        <button type="button" className={form.primaryButton} onClick={onSave} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </React.Fragment>
    )}
  </div>
);

export default WorkspaceSettingsModal;
