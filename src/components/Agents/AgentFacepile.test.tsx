import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import AgentFacepile, { facepileOrder } from './AgentFacepile';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';

const agent = (id: string, over: Partial<WorkspaceAgentResponse> = {}): WorkspaceAgentResponse => ({
  id,
  workspaceId: 'ws-1',
  agentDefinitionId: `def-${id}`,
  displayName: id,
  role: 'member',
  enabled: true,
  isDefault: false,
  agentName: null,
  agentDescription: null,
  providerConnectionIds: [],
  skillIds: [],
  selectedMcpServerIds: [],
  execution: {},
  avatar: null,
  createdAt: 1n,
  updatedAt: 1n,
  ...over,
});

const running = (agentId: string): WorkspaceTaskResponse =>
  ({
    id: `t-${agentId}`,
    assignedToWorkspaceAgentId: agentId,
    status: 'running',
  }) as WorkspaceTaskResponse;

describe('facepileOrder', () => {
  it('puts the Main first, then whoever is working, then the rest, disabled last', () => {
    const agents = [
      agent('idle'),
      agent('off', { enabled: false }),
      agent('busy'),
      agent('main', { isDefault: true }),
    ];
    expect(facepileOrder(agents, [running('busy')]).map((entry) => entry.agent.id)).toEqual([
      'main',
      'busy',
      'idle',
      'off',
    ]);
  });
});

describe('AgentFacepile', () => {
  it('shows at most `max` faces and counts the rest, ringing only the working ones', () => {
    const agents = [
      agent('main', { isDefault: true }),
      agent('a'),
      agent('b'),
      agent('c'),
      agent('d'),
      agent('e'),
    ];
    const { container } = render(<AgentFacepile agents={agents} tasks={[running('c')]} max={4} />);
    const faces = container.querySelectorAll('[data-activity]');
    expect(faces).toHaveLength(4);
    expect(Array.from(faces).map((face) => face.getAttribute('data-activity'))).toEqual([
      'none',
      'running',
      'none',
      'none',
    ]);
    expect(screen.getByText('+2')).toBeInTheDocument();
  });

  it('draws no "+N" when everyone fits', () => {
    render(<AgentFacepile agents={[agent('a'), agent('b')]} tasks={[]} />);
    expect(screen.queryByText(/^\+\d+$/)).toBeNull();
  });
});
