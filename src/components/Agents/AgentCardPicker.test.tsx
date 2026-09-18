import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const api = vi.hoisted(() => ({
  assignWorkspaceAgent: vi.fn(),
  workspaceDeleteAgent: vi.fn(),
}));
vi.mock('../../api/client', () => api);

import AgentCardPicker, { availableDefinitions, cardFacts } from './AgentCardPicker';
import type { AgentDefinitionDetail } from '../../api/client';

const definition = (over: Partial<AgentDefinitionDetail>): AgentDefinitionDetail => ({
  id: 'def',
  revision: 1,
  archived: false,
  name: 'Agent',
  description: '',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: [],
  execution: {},
  enabled: true,
  avatar: null,
  createdAt: 1,
  updatedAt: 1,
  assignedWorkspaces: [],
  ...over,
});

const REVIEWER = definition({
  id: 'def-review',
  name: 'Reviewer',
  selectedSkillIds: ['a', 'b', 'c'],
});
const WRITER = definition({
  id: 'def-writer',
  name: 'Writer',
  description: 'Writes docs',
  selectedMcpServerIds: ['m'],
});
const RETIRED = definition({ id: 'def-retired', name: 'Retired', archived: true });
const LIBRARY = [REVIEWER, WRITER, RETIRED];

beforeEach(() => {
  api.assignWorkspaceAgent.mockReset().mockResolvedValue('assign-2');
  api.workspaceDeleteAgent.mockReset().mockResolvedValue(undefined);
});

describe('availableDefinitions', () => {
  it('drops archived definitions and those already on the crew', () => {
    expect(availableDefinitions(LIBRARY, ['def-review']).map((d) => d.id)).toEqual(['def-writer']);
  });
});

describe('cardFacts', () => {
  it('names skills and MCP servers, pluralised, and says when there are none', () => {
    expect(cardFacts(REVIEWER)).toBe('3 skills');
    expect(cardFacts(WRITER)).toBe('1 MCP server');
    expect(cardFacts(RETIRED)).toBe('No skills or MCP servers');
  });
});

describe('AgentCardPicker', () => {
  it('offers a card per agent still off the crew and shows who is already on it', () => {
    render(
      <AgentCardPicker
        workspaceId="ws-1"
        definitions={LIBRARY}
        assignedDefinitionIds={['def-review']}
      />
    );

    expect(screen.getByRole('listitem', { name: 'Add Writer to the crew' })).toBeInTheDocument();
    expect(screen.queryByRole('listitem', { name: /Reviewer/ })).toBeNull();
    expect(screen.queryByRole('listitem', { name: /Retired/ })).toBeNull();
    expect(screen.getByRole('img', { name: 'Reviewer' })).toBeInTheDocument();
  });

  it('assigns on a single click, exactly once, then offers Undo', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    render(
      <AgentCardPicker
        workspaceId="ws-1"
        definitions={LIBRARY}
        assignedDefinitionIds={[]}
        onChanged={onChanged}
      />
    );

    await user.click(screen.getByRole('listitem', { name: 'Add Writer to the crew' }));

    await waitFor(() =>
      expect(api.assignWorkspaceAgent).toHaveBeenCalledWith('ws-1', 'def-writer')
    );
    expect(api.assignWorkspaceAgent).toHaveBeenCalledTimes(1);
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('status')).toHaveTextContent('Writer joined the crew.');

    await user.click(screen.getByRole('button', { name: 'Undo' }));
    await waitFor(() => expect(api.workspaceDeleteAgent).toHaveBeenCalledWith('ws-1', 'assign-2'));
    expect(onChanged).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('ignores a second click while the first assign is in flight', async () => {
    const user = userEvent.setup();
    let release: (id: string) => void = () => {};
    api.assignWorkspaceAgent.mockImplementation(
      () => new Promise<string>((resolve) => (release = resolve))
    );
    render(<AgentCardPicker workspaceId="ws-1" definitions={LIBRARY} assignedDefinitionIds={[]} />);

    const writer = screen.getByRole('listitem', { name: 'Add Writer to the crew' });
    await user.click(writer);
    expect(writer).toBeDisabled();
    await user.click(screen.getByRole('listitem', { name: 'Add Reviewer to the crew' }));
    expect(api.assignWorkspaceAgent).toHaveBeenCalledTimes(1);

    release('assign-9');
    await waitFor(() => expect(screen.getByRole('status')).toBeInTheDocument());
  });

  it('shows the error and keeps the cards when the assign fails', async () => {
    const user = userEvent.setup();
    api.assignWorkspaceAgent.mockRejectedValue(new Error('backend said no'));
    render(<AgentCardPicker workspaceId="ws-1" definitions={LIBRARY} assignedDefinitionIds={[]} />);

    await user.click(screen.getByRole('listitem', { name: 'Add Writer to the crew' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('backend said no');
    expect(screen.getByRole('listitem', { name: 'Add Writer to the crew' })).toBeEnabled();
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('tells the empty library from the fully assigned one and links to Create', async () => {
    const user = userEvent.setup();
    const onCreateAgent = vi.fn();
    const { rerender } = render(
      <AgentCardPicker
        workspaceId="ws-1"
        definitions={[]}
        assignedDefinitionIds={[]}
        onCreateAgent={onCreateAgent}
      />
    );
    expect(screen.getByText('The library is empty.')).toBeInTheDocument();

    rerender(
      <AgentCardPicker
        workspaceId="ws-1"
        definitions={[REVIEWER, WRITER]}
        assignedDefinitionIds={['def-review', 'def-writer']}
        onCreateAgent={onCreateAgent}
      />
    );
    expect(
      screen.getByText('Every agent in the library is already on this crew.')
    ).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Create a new agent →' }));
    expect(onCreateAgent).toHaveBeenCalled();
  });
});
