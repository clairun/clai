import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// The real form inside the real window: a stale save is recovered through
// the window's Discard, so neither half alone can show it works.
const api = vi.hoisted(() => ({
  getMcpServers: vi.fn(),
  getSkills: vi.fn(),
  workspaceAgentDefaultExecution: vi.fn(),
  workspaceGetAgent: vi.fn(),
  workspaceCreateAgent: vi.fn(),
  workspaceUpdateAgent: vi.fn(),
  workspaceDeleteAgent: vi.fn(),
  getSandboxStatus: vi.fn(),
}));
vi.mock('../../api/client', () => api);
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(null) }));
vi.mock('../../assistant', () => ({
  assistantClient: {
    listProviderConnections: vi.fn().mockResolvedValue([{ id: 'conn-a', name: 'Anthropic', enabled: true, modelId: 'sonnet' }]),
  },
}));
vi.mock('../../workspace/client', () => ({ setWorkspaceTitle: vi.fn() }));

import WorkspaceSettingsModal from './WorkspaceSettingsModal';
import { StaleAgentError } from '../../api/errors';
import { resetSandboxStatusForTests } from '../../hooks/useSandboxStatus';
import type { WorkspaceDetails } from '../../generated/bindings';
import type { AgentDetail } from './AgentBehaviorForm';

const STALE = 'This agent was changed elsewhere after you opened it. Use Discard to load the latest settings.';

const MAIN: AgentDetail = {
  id: 'agent-main',
  name: 'Manager',
  description: 'Reviews diffs',
  isDefault: true,
  enabled: true,
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: ['conn-a'],
  execution: {
    sandbox: { networkAccess: { mode: 'host_network', allowDomains: [], denyDomains: [] }, sessionBusMode: 'full' },
    filesystem: { extraPaths: [] },
    shell: { mode: 'restricted', askFirstCommandPrefixes: ['git push'] },
    web: { enabled: true },
  },
  updatedAt: 5,
};

const DETAILS = {
  workspaceId: 'ws-1',
  title: 'Backend',
  assignedAgents: [{ id: 'agent-main', isDefault: true, displayName: 'Main', enabled: true, providerConnectionIds: [] }],
  defaultWorkspaceAgentId: 'agent-main',
  scheduleEnabled: false,
} as unknown as WorkspaceDetails;

const saveBar = () => screen.queryByRole('region', { name: 'Unsaved changes' });

describe('WorkspaceSettingsModal: a Main save refused as stale', () => {
  beforeEach(() => {
    api.getMcpServers.mockReset().mockResolvedValue([]);
    api.getSkills.mockReset().mockResolvedValue([]);
    api.workspaceAgentDefaultExecution.mockReset().mockResolvedValue(null);
    api.workspaceGetAgent.mockReset();
    api.workspaceUpdateAgent.mockReset();
    api.getSandboxStatus.mockReset().mockResolvedValue({
      available: true, backend: 'bubblewrap', allowlistAvailable: true, allowlistUnavailableReason: null,
      socketIsolationAvailable: true, socketIsolationUnavailableReason: null,
      sessionBusFilterAvailable: true, sessionBusFilterUnavailableReason: null,
    });
    resetSandboxStatusForTests();
  });

  it('shows the refusal in the save bar once, and Discard clears it and loads the agent as it is now', async () => {
    const user = userEvent.setup();
    // An Always approval landed while the form was open.
    const approved = { ...MAIN, execution: { ...MAIN.execution, shell: { mode: 'restricted', askFirstCommandPrefixes: [] } }, updatedAt: 9 };
    api.workspaceGetAgent.mockResolvedValueOnce(MAIN).mockResolvedValueOnce(approved);
    api.workspaceUpdateAgent.mockRejectedValueOnce(new StaleAgentError(STALE)).mockResolvedValue(approved);
    render(
      <WorkspaceSettingsModal
        isOpen
        onClose={vi.fn()}
        workspaceId="ws-1"
        details={DETAILS}
        initialSelection={{ kind: 'agent', agentId: 'agent-main' }}
      />,
    );

    await user.type(await screen.findByLabelText('Instructions'), ' and tests');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));

    expect(await within(saveBar()!).findByRole('alert')).toHaveTextContent(STALE);
    expect(within(saveBar()!).getAllByText(STALE)).toHaveLength(1);
    expect(screen.queryByRole('button', { name: /Reload/ })).toBeNull();

    await user.click(within(saveBar()!).getByRole('button', { name: 'Discard' }));

    expect(await screen.findByDisplayValue('Reviews diffs')).toBeInTheDocument();
    expect(api.workspaceGetAgent).toHaveBeenCalledTimes(2);
    expect(saveBar()).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByRole('heading', { level: 2, name: 'Main' })).toHaveFocus();

    // The next save is based on the agent as it is now, approval included.
    await user.type(screen.getByLabelText('Instructions'), ' and tests');
    await user.click(within(saveBar()!).getByRole('button', { name: 'Save' }));
    expect(api.workspaceUpdateAgent).toHaveBeenLastCalledWith(expect.objectContaining({
      expectedUpdatedAt: 9,
      description: 'Reviews diffs and tests',
      execution: expect.objectContaining({ shell: { mode: 'restricted', askFirstCommandPrefixes: [] } }),
    }));
  });
});
