import { describe, expect, it, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
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
vi.mock('./AgentBehaviorForm', () => ({ AgentBehaviorForm: () => <div data-testid="agent-form" /> }));
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
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const { onClose } = renderStack();

    await user.type(await screen.findByLabelText(/Title/), ' rewrite');
    act(() => openGlobalSettings({ tab: 'agents' }));

    expect(confirm).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    confirm.mockRestore();
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
