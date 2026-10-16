import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect, useState } from 'react';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(null) }));
vi.mock('../../api/client', () => ({
  getMcpServers: vi.fn().mockResolvedValue([]),
  getSkills: vi.fn().mockResolvedValue([]),
  workspaceAgentDefaultExecution: vi.fn().mockResolvedValue(null),
  workspaceCreateAgent: vi.fn().mockResolvedValue({ id: 'agent-new' }),
  workspaceUpdateAgent: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../../assistant', () => ({
  assistantClient: { listProviderConnections: vi.fn().mockResolvedValue([]) },
}));
vi.mock('../../workspace/client', () => ({ setWorkspaceTitle: vi.fn() }));
// The sections have their own tests; here they are stubs that keep the page
// contract (a handle, dirty on input) so what is under test is the shell.
vi.mock('./WorkspaceTeamSettings', async () => {
  const { useImperativeHandle } = await import('react');
  const DraftPage = ({ ref, onDirtyChange, label }: PageStubProps & { label: string }) => {
    useImperativeHandle(ref, () => ({ validate: () => ({ ok: true }), submit: async () => ({ ok: true }) }));
    return <input aria-label={label} onChange={() => onDirtyChange?.(true)} />;
  };
  return {
    TeamPolicySection: (props: PageStubProps) => <DraftPage {...props} label="Team context" />,
    AssignmentSection: ({ agentId, onUnassigned, ...props }: PageStubProps & {
      agentId?: string;
      onUnassigned?: (id: string) => void;
    }) => (
      <>
        <DraftPage {...props} label="Assignment context" />
        <button type="button" onClick={() => onUnassigned?.(agentId!)}>Remove from crew</button>
      </>
    ),
  };
});
// Same contract as the real form: no agentId means Save creates one.
vi.mock('./AgentBehaviorForm', async () => {
  const { useImperativeHandle } = await import('react');
  const api = await import('../../api/client');
  return {
    AgentBehaviorForm: ({ ref, agentId, onDirtyChange }: PageStubProps & { agentId: string | null }) => {
      useImperativeHandle(ref, () => ({
        validate: () => ({ ok: true }),
        submit: async () => {
          if (!agentId) return { ok: true, createdId: (await api.workspaceCreateAgent({} as never)).id };
          await api.workspaceUpdateAgent({ agentId } as never);
          onDirtyChange?.(false);
          return { ok: true };
        },
      }));
      return (
        <>
          <input type="search" aria-label="Search skills" defaultValue="rust" />
          <input aria-label="Agent name" onChange={() => onDirtyChange?.(true)} />
        </>
      );
    },
  };
});
// The global modal's tabs are heavy modules this test does not exercise; the
// modal shell itself is real, because its overlay is what stacks.
vi.mock('./AssistantProviderSettings', () => ({ default: () => <div /> }));
vi.mock('./McpServersSettings', () => ({ default: () => <div /> }));
vi.mock('./SkillsSettings', () => ({ default: () => <div /> }));
vi.mock('./AppearanceSettings', () => ({ default: () => <div /> }));
vi.mock('./ApplicationsSettings', () => ({ default: () => <div /> }));
vi.mock('./AboutSettings', () => ({ default: () => <div /> }));
vi.mock('./AgentLibrarySettings', () => ({ default: () => <div data-testid="library" /> }));

import WorkspaceSettingsModal from './WorkspaceSettingsModal';
import SettingsModal, { TABS } from './SettingsModal';
import { OPEN_GLOBAL_SETTINGS_EVENT, openGlobalSettings } from '../../utils/globalSettings';
import type { WorkspaceDetails } from '../../generated/bindings';
import type { SectionHandle } from './sectionHandle';
import { invoke } from '@tauri-apps/api/core';
import { setWorkspaceTitle } from '../../workspace/client';
import { workspaceCreateAgent, workspaceUpdateAgent } from '../../api/client';

interface PageStubProps {
  ref?: React.Ref<SectionHandle>;
  onDirtyChange?: (isDirty: boolean) => void;
}

const DETAILS = {
  workspaceId: 'ws-1',
  title: 'Backend',
  assignedAgents: [],
  defaultWorkspaceAgentId: 'agent-main',
  scheduleEnabled: false,
} as unknown as WorkspaceDetails;

/**
 * The FleetLayout arrangement: the workspace modal open, and the global
 * Settings modal opened over it by the same event a crew-member link fires.
 */
const renderStack = () => {
  const onClose = vi.fn();
  const onGlobalClose = vi.fn();

  const Host = () => {
    const [globalOpen, setGlobalOpen] = useState(false);
    useEffect(() => {
      const open = () => setGlobalOpen(true);
      window.addEventListener(OPEN_GLOBAL_SETTINGS_EVENT, open);
      return () => window.removeEventListener(OPEN_GLOBAL_SETTINGS_EVENT, open);
    }, []);
    return (
      <>
        <WorkspaceSettingsModal
          isOpen
          onClose={onClose}
          workspaceId="ws-1"
          details={DETAILS}
          initialSelection={{ kind: 'general' }}
        />
        <SettingsModal
          isOpen={globalOpen}
          onClose={() => {
            onGlobalClose();
            setGlobalOpen(false);
          }}
          initialTab={TABS.AGENTS}
        />
      </>
    );
  };

  render(<Host />);
  return { onClose, onGlobalClose };
};

const LEAVE_PROMPT = 'Save your changes before leaving?';

const globalModalIsUp = () => screen.queryByRole('heading', { name: 'Settings' }) !== null;

describe('WorkspaceSettingsModal under the global Settings modal', () => {
  it('stays open when a deep link opens the global modal over it', async () => {
    const { onClose } = renderStack();
    await screen.findByLabelText(/Title/);

    act(() => openGlobalSettings({ tab: 'agents', agentDefinitionId: 'def-1' }));

    // It used to hand over by closing itself; the modals stack now, so the
    // workspace draft is still here when the library closes.
    expect(onClose).not.toHaveBeenCalled();
    expect(globalModalIsUp()).toBe(true);
    expect(screen.getByLabelText(/Title/)).toBeInTheDocument();
  });

  it('does not prompt about unsaved changes when the global modal opens', async () => {
    const user = userEvent.setup();
    const { onClose } = renderStack();

    await user.type(await screen.findByLabelText(/Title/), ' rewrite');
    act(() => openGlobalSettings({ tab: 'agents' }));

    expect(screen.queryByText(LEAVE_PROMPT)).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('leaves Escape to the modal stacked over it, and takes it back after', async () => {
    const user = userEvent.setup();
    const { onClose, onGlobalClose } = renderStack();
    await screen.findByLabelText(/Title/);

    act(() => openGlobalSettings({ tab: 'agents' }));
    expect(globalModalIsUp()).toBe(true);

    await user.keyboard('{Escape}');
    // Only the topmost one: closing both would drop the user out of the
    // workspace settings they never asked to leave.
    expect(onGlobalClose).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
    expect(globalModalIsUp()).toBe(false);

    // Back to the index first, then closed.
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /General/ })).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('keeps the page locked while the global modal closes over it', async () => {
    const user = userEvent.setup();
    renderStack();
    await screen.findByLabelText(/Title/);

    act(() => openGlobalSettings({ tab: 'agents' }));
    await user.keyboard('{Escape}');

    // The workspace modal is still up: the last overlay to close owns the
    // unlock, not the first.
    expect(document.body.style.overflow).toBe('hidden');
  });
});

const MAIN = {
  id: 'agent-main',
  isDefault: true,
  displayName: 'Main',
  enabled: true,
  skillIds: ['a', 'b', 'c'],
  selectedMcpServerIds: ['m'],
  providerConnectionIds: [],
};

const CREW = {
  ...MAIN,
  id: 'agent-crew',
  isDefault: false,
  displayName: 'Reviewer',
};

const renderModal = (initialSelection?: { kind: 'general' | 'schedule' | 'team' | 'agent'; agentId?: string }) => {
  const onClose = vi.fn();
  render(
    <WorkspaceSettingsModal
      isOpen
      onClose={onClose}
      workspaceId="ws-1"
      details={{ ...DETAILS, assignedAgents: [MAIN, CREW] } as unknown as WorkspaceDetails}
      initialSelection={initialSelection}
    />,
  );
  return { onClose };
};

const pageHeading = (name: string) => screen.getByRole('heading', { level: 2, name });
const overlayOf = (heading: HTMLElement) => heading.closest('header')!.parentElement!.parentElement!;
const saveBar = () => screen.queryByRole('region', { name: 'Unsaved changes' });

describe('WorkspaceSettingsModal navigation and per-page save', () => {
  beforeEach(() => {
    vi.mocked(setWorkspaceTitle).mockReset();
    vi.mocked(invoke).mockClear();
  });

  it('opens on the index, enters a page, and returns focus to its row', async () => {
    const user = userEvent.setup();
    renderModal();

    expect(screen.getByRole('button', { name: /Main.*3 skills · 1 MCP/ })).toBeInTheDocument();
    const row = screen.getByRole('button', { name: /General/ });
    await user.click(row);
    expect(pageHeading('General')).toHaveFocus();

    await user.click(screen.getByRole('button', { name: /Backend — Settings/ }));
    expect(screen.getByRole('button', { name: /General/ })).toHaveFocus();
  });

  it('closes on Escape at the index but goes back one level inside a page', async () => {
    const user = userEvent.setup();
    const { onClose } = renderModal({ kind: 'schedule' });

    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /Schedule/ })).toHaveFocus();

    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('lets a focused search field with text take Escape first', async () => {
    const user = userEvent.setup();
    const { onClose } = renderModal({ kind: 'agent', agentId: 'agent-main' });
    const search = screen.getByRole('searchbox', { name: 'Search skills' });

    search.focus();
    await user.keyboard('{Escape}');
    expect(pageHeading('Main')).toBeInTheDocument();

    await user.clear(search);
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('searchbox')).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('shows the save bar only while the page has changes', async () => {
    const user = userEvent.setup();
    renderModal({ kind: 'general' });
    const title = screen.getByLabelText(/Title/);

    expect(saveBar()).not.toBeInTheDocument();
    await user.type(title, ' API');
    expect(saveBar()).toBeInTheDocument();

    await user.click(within(saveBar()!).getByRole('button', { name: 'Discard' }));
    expect(saveBar()).not.toBeInTheDocument();
    expect(screen.getByLabelText(/Title/)).toHaveValue('Backend');
  });

  it('asks before leaving a dirty page: Stay keeps it, Discard leaves without writing', async () => {
    const user = userEvent.setup();
    const { onClose } = renderModal({ kind: 'general' });
    await user.type(screen.getByLabelText(/Title/), ' API');

    const back = screen.getByRole('button', { name: /Backend — Settings/ });
    await user.click(back);
    expect(screen.getByText(LEAVE_PROMPT)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Stay' }));
    expect(screen.getByLabelText(/Title/)).toHaveValue('Backend API');
    // Focus goes back to what asked to leave, not to <body>.
    expect(back).toHaveFocus();

    // Escape also asks, and a second Escape answers Stay.
    await user.keyboard('{Escape}');
    expect(screen.getByText(LEAVE_PROMPT)).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(screen.queryByText(LEAVE_PROMPT)).not.toBeInTheDocument();
    expect(screen.getByLabelText(/Title/)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Close settings' }));
    await user.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(setWorkspaceTitle).not.toHaveBeenCalled();
  });

  it('saves from the leave prompt, then leaves', async () => {
    const user = userEvent.setup();
    renderModal({ kind: 'general' });
    await user.type(screen.getByLabelText(/Title/), ' API');

    await user.click(screen.getByRole('button', { name: /Backend — Settings/ }));
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(setWorkspaceTitle).toHaveBeenCalledWith('ws-1', 'Backend API');
    expect(screen.getByRole('button', { name: /General/ })).toBeInTheDocument();
  });

  it('saves only the visible page', async () => {
    const user = userEvent.setup();
    renderModal({ kind: 'general' });
    await user.type(screen.getByLabelText(/Title/), ' API');

    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(setWorkspaceTitle).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalledWith('workspace_set_schedule', expect.anything());
    // Still on the page after a save, with focus on it rather than <body>.
    expect(pageHeading('General')).toHaveFocus();
  });

  it('keeps the page open and shows why when validation fails', async () => {
    const user = userEvent.setup();
    renderModal({ kind: 'general' });
    await user.clear(screen.getByLabelText(/Title/));

    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(within(saveBar()!).getByRole('alert')).toHaveTextContent('Workspace title cannot be empty.');
    expect(setWorkspaceTitle).not.toHaveBeenCalled();
  });

  it('asks before an overlay click closes a dirty page', async () => {
    const user = userEvent.setup();
    const { onClose } = renderModal({ kind: 'general' });
    await user.type(screen.getByLabelText(/Title/), ' API');

    await user.click(overlayOf(pageHeading('General')));

    expect(screen.getByText(LEAVE_PROMPT)).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('stays on the page when Save from the leave prompt fails, and Discard still leaves', async () => {
    const user = userEvent.setup();
    vi.mocked(setWorkspaceTitle).mockRejectedValue(new Error('disk full'));
    const { onClose } = renderModal({ kind: 'general' });
    await user.type(screen.getByLabelText(/Title/), ' API');

    await user.click(screen.getByRole('button', { name: 'Close settings' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(onClose).not.toHaveBeenCalled();
    expect(pageHeading('General')).toBeInTheDocument();
    expect(within(saveBar()!).getByRole('alert')).toHaveTextContent('disk full');

    await user.click(screen.getByRole('button', { name: 'Close settings' }));
    await user.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it.each([
    ['Team', { kind: 'team' as const }, 'Team context'],
    ['a crew member', { kind: 'agent' as const, agentId: 'agent-crew' }, 'Assignment context'],
  ])('asks before leaving a dirty %s page', async (_name, selection, field) => {
    const user = userEvent.setup();
    renderModal(selection);
    await user.type(screen.getByLabelText(field), 'x');

    await user.keyboard('{Escape}');

    expect(screen.getByText(LEAVE_PROMPT)).toBeInTheDocument();
    expect(screen.getByLabelText(field)).toBeInTheDocument();
  });

  it('continues on the created Main, so Discard then Save does not create a second one', async () => {
    const user = userEvent.setup();
    vi.mocked(workspaceCreateAgent).mockClear();
    vi.mocked(workspaceUpdateAgent).mockClear();
    const Host = () => {
      const [details, setDetails] = useState({ ...DETAILS, defaultWorkspaceAgentId: null } as unknown as WorkspaceDetails);
      return (
        <WorkspaceSettingsModal
          isOpen
          onClose={() => {}}
          workspaceId="ws-1"
          details={details}
          onChanged={() => setDetails({
            ...details,
            defaultWorkspaceAgentId: 'agent-new',
            assignedAgents: [{ ...MAIN, id: 'agent-new' }],
          } as unknown as WorkspaceDetails)}
        />
      );
    };
    render(<Host />);

    await user.click(screen.getByRole('button', { name: /Main — set up/ }));
    await user.type(screen.getByLabelText('Agent name'), 'Main');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));
    expect(workspaceCreateAgent).toHaveBeenCalledTimes(1);
    expect(pageHeading('Main')).toHaveFocus();

    await user.type(screen.getByLabelText('Agent name'), ' edit');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Discard' }));
    await user.type(screen.getByLabelText('Agent name'), ' again');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(workspaceCreateAgent).toHaveBeenCalledTimes(1);
    expect(workspaceUpdateAgent).toHaveBeenCalledWith({ agentId: 'agent-new' });

    // Back lands on the new Main's row; the "set up" row is gone.
    await user.click(screen.getByRole('button', { name: /Backend — Settings/ }));
    expect(screen.getByRole('button', { name: /^Main/ })).toHaveFocus();
  });

  it('is already on the created Main while the refetch runs, so a second Save updates it', async () => {
    const user = userEvent.setup();
    vi.mocked(workspaceCreateAgent).mockClear();
    vi.mocked(workspaceUpdateAgent).mockClear();
    let finishRefetch = () => {};
    render(
      <WorkspaceSettingsModal
        isOpen
        onClose={() => {}}
        workspaceId="ws-1"
        details={{ ...DETAILS, defaultWorkspaceAgentId: null } as unknown as WorkspaceDetails}
        onChanged={() => new Promise<void>((resolve) => { finishRefetch = resolve; })}
      />,
    );

    await user.click(screen.getByRole('button', { name: /Main — set up/ }));
    await user.type(screen.getByLabelText('Agent name'), 'Main');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));
    await user.type(screen.getByLabelText('Agent name'), ' edit');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(workspaceCreateAgent).toHaveBeenCalledTimes(1);
    expect(workspaceUpdateAgent).toHaveBeenCalledWith({ agentId: 'agent-new' });
    await act(async () => finishRefetch());
  });

  it('keeps focus inside the modal when it opens on the index or the origin row is gone', async () => {
    const user = userEvent.setup();
    renderModal();
    expect(screen.getByRole('button', { name: /General/ })).toHaveFocus();

    await user.click(screen.getByRole('button', { name: /Reviewer/ }));
    await user.click(screen.getByRole('button', { name: 'Remove from crew' }));

    expect(screen.getByRole('button', { name: /General/ })).toHaveFocus();
  });
});
