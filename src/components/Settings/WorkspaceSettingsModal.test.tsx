import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect, useState } from 'react';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(null) }));
vi.mock('../../api/client', () => ({
  getMcpServers: vi.fn().mockResolvedValue([]),
  getSkills: vi.fn().mockResolvedValue([]),
  workspaceAgentDefaultExecution: vi.fn().mockResolvedValue(null),
}));
vi.mock('../../assistant', () => ({
  assistantClient: { listProviderConnections: vi.fn().mockResolvedValue([]) },
}));
vi.mock('../../workspace/client', () => ({ setWorkspaceTitle: vi.fn() }));
// The sections have their own tests; here they are stubs so what is under
// test is the shell's own behaviour.
vi.mock('./WorkspaceTeamSettings', () => ({
  AssignmentSection: () => <div data-testid="assignment-section" />,
  TeamPolicySection: () => <div data-testid="team-section" />,
}));
vi.mock('./AgentBehaviorForm', () => ({
  AgentBehaviorForm: () => <input type="search" aria-label="Search skills" defaultValue="rust" />,
}));
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
import { invoke } from '@tauri-apps/api/core';
import { setWorkspaceTitle } from '../../workspace/client';

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

const renderModal = (initialSelection?: { kind: 'general' | 'schedule' | 'agent'; agentId?: string }) => {
  const onClose = vi.fn();
  render(
    <WorkspaceSettingsModal
      isOpen
      onClose={onClose}
      workspaceId="ws-1"
      details={{ ...DETAILS, assignedAgents: [MAIN] } as unknown as WorkspaceDetails}
      initialSelection={initialSelection}
    />,
  );
  return { onClose };
};

const pageHeading = (name: string) => screen.getByRole('heading', { level: 2, name });
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

    await user.click(screen.getByRole('button', { name: /Backend — Settings/ }));
    expect(screen.getByText(LEAVE_PROMPT)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Stay' }));
    expect(screen.getByLabelText(/Title/)).toHaveValue('Backend API');

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
    // Still on the page after a save.
    expect(pageHeading('General')).toBeInTheDocument();
  });

  it('keeps the page open and shows why when validation fails', async () => {
    const user = userEvent.setup();
    renderModal({ kind: 'general' });
    await user.clear(screen.getByLabelText(/Title/));

    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(within(saveBar()!).getByRole('alert')).toHaveTextContent('Workspace title cannot be empty.');
    expect(setWorkspaceTitle).not.toHaveBeenCalled();
  });
});
