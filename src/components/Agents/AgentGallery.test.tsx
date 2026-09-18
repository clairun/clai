import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import AgentGallery, { galleryOrder, usageSentence } from './AgentGallery';
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

const WRITER = definition({
  id: 'def-writer',
  name: 'Writer',
  description: 'Writes release notes',
});
const REVIEWER = definition({
  id: 'def-review',
  name: 'Reviewer',
  description: 'Reviews PRs',
  assignedWorkspaces: [
    { id: 'ws-1', title: 'A' },
    { id: 'ws-2', title: 'B' },
  ] as never,
});
const ARCHIVED = definition({ id: 'def-old', name: 'Alpha', archived: true });

describe('galleryOrder', () => {
  it('sorts live agents by name and archived ones last', () => {
    expect(galleryOrder([WRITER, ARCHIVED, REVIEWER]).map((d) => d.id)).toEqual([
      'def-review',
      'def-writer',
      'def-old',
    ]);
  });

  it('matches the query against name or description, case-insensitively', () => {
    expect(galleryOrder([WRITER, ARCHIVED, REVIEWER], 'NOTES').map((d) => d.id)).toEqual([
      'def-writer',
    ]);
    expect(galleryOrder([WRITER, ARCHIVED, REVIEWER], 'alp').map((d) => d.id)).toEqual(['def-old']);
  });
});

describe('usageSentence', () => {
  it('reads as a sentence in every state', () => {
    expect(usageSentence(WRITER)).toBe('Not on any workspace');
    expect(usageSentence(REVIEWER)).toBe('On 2 workspaces');
    expect(
      usageSentence({ ...REVIEWER, assignedWorkspaces: REVIEWER.assignedWorkspaces.slice(0, 1) })
    ).toBe('On 1 workspace');
    expect(usageSentence(ARCHIVED)).toBe('Archived');
  });
});

describe('AgentGallery', () => {
  it('opens the editor for the clicked card and the create flow from the button, not a card', async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    const onCreate = vi.fn();
    render(<AgentGallery definitions={[WRITER, REVIEWER]} onOpen={onOpen} onCreate={onCreate} />);

    expect(screen.getAllByRole('button', { name: /^(Writer|Reviewer)/ })).toHaveLength(2);
    await user.click(screen.getByRole('button', { name: /^Reviewer/ }));
    expect(onOpen).toHaveBeenCalledWith('def-review');
    await user.click(screen.getByRole('button', { name: '+ Create agent' }));
    expect(onCreate).toHaveBeenCalledTimes(1);
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it('narrows the grid with the search box and says when nothing matches', async () => {
    const user = userEvent.setup();
    render(<AgentGallery definitions={[WRITER, REVIEWER]} onOpen={vi.fn()} onCreate={vi.fn()} />);
    await user.type(screen.getByRole('searchbox', { name: 'Search agents' }), 'writ');
    expect(screen.getAllByRole('button', { name: /^(Writer|Reviewer)/ })).toHaveLength(1);
    await user.type(screen.getByRole('searchbox', { name: 'Search agents' }), 'zzz');
    expect(screen.getByText('No agent matches that search.')).toBeInTheDocument();
  });

  it('focuses the card asked for', () => {
    render(
      <AgentGallery
        definitions={[WRITER, REVIEWER]}
        onOpen={vi.fn()}
        onCreate={vi.fn()}
        focusId="def-writer"
      />
    );
    expect(screen.getByRole('button', { name: /^Writer/ })).toHaveFocus();
  });

  it('invites creation when the library is empty, without a search box', () => {
    render(<AgentGallery definitions={[]} onOpen={vi.fn()} onCreate={vi.fn()} />);
    expect(screen.getByText('No agents yet. Create the first one.')).toBeInTheDocument();
    expect(screen.queryByRole('searchbox')).toBeNull();
  });
});
