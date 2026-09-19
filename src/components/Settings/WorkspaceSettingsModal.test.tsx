import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

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

import WorkspaceSettingsModal from './WorkspaceSettingsModal';
import { openGlobalSettings } from '../../utils/globalSettings';
import type { WorkspaceSnapshot } from '../../generated/bindings';

const SNAPSHOT = {
  workspaceId: 'ws-1',
  title: 'Backend',
  assignedAgents: [],
  defaultWorkspaceAgentId: 'agent-main',
  scheduleEnabled: false,
} as unknown as WorkspaceSnapshot;

const renderModal = () => {
  const onClose = vi.fn();
  render(
    <WorkspaceSettingsModal
      isOpen
      onClose={onClose}
      workspaceId="ws-1"
      snapshot={SNAPSHOT}
      initialSelection={{ kind: 'general' }}
    />
  );
  return onClose;
};

afterEach(() => {
  vi.restoreAllMocks();
});

describe('WorkspaceSettingsModal global-settings hand-off', () => {
  it('closes so the global modal is not opened underneath it', async () => {
    const onClose = renderModal();
    await screen.findByLabelText(/Title/);

    openGlobalSettings({ tab: 'agents', agentDefinitionId: 'def-1' });

    // Both modals are siblings and this one sits at the higher z-index, so
    // the only way the user reaches the global one is if this one leaves.
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('keeps unsaved changes when the leave prompt is declined', async () => {
    const user = userEvent.setup();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const onClose = renderModal();

    await user.type(await screen.findByLabelText(/Title/), ' rewrite');
    openGlobalSettings({ tab: 'agents' });

    expect(confirm).toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    openGlobalSettings({ tab: 'agents' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
