import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createRef } from 'react';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
const mockAsk = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/plugin-dialog', () => ({ ask: mockAsk }));

import { CraftView } from './AgentCraft';
import type { AgentCraftFile } from '../../generated/bindings';

const lesson = (description: string, updatedAt: string, body = 'Body\n') =>
  `---\ndescription: ${description}\nupdated_at: ${updatedAt}\n---\n${body}`;

const TESTS: AgentCraftFile = {
  path: 'react/tests.md',
  description: 'Use when testing focus: fire keys at the focused element.',
  updatedAt: '2026-11-14',
  frontmatterValid: true,
};
const BROKEN: AgentCraftFile = { path: 'broken.md', description: null, updatedAt: null, frontmatterValid: false };

// The craft folder as the backend keeps it: `list` and `read` see what
// `write` and `delete` did.
let disk: Map<string, { meta: AgentCraftFile; content: string }>;

const seed = (...files: [AgentCraftFile, string][]) => {
  disk = new Map(files.map(([meta, content]) => [meta.path, { meta, content }]));
};

beforeEach(() => {
  seed([TESTS, lesson(TESTS.description!, '2026-11-14')], [BROKEN, 'no frontmatter\n']);
  mockInvoke.mockReset();
  mockAsk.mockReset();
  mockInvoke.mockImplementation(async (cmd: string, args: { definitionId: string; path?: string; content?: string }) => {
    expect(args.definitionId).toBe('def-1');
    switch (cmd) {
      case 'agent_craft_list':
        return [...disk.values()].map((file) => file.meta);
      case 'agent_craft_read':
        return disk.get(args.path!)!.content;
      case 'agent_craft_write': {
        const description = /description: (.*)/.exec(args.content!)?.[1] ?? null;
        const meta = disk.get(args.path!)!.meta;
        disk.set(args.path!, { meta: { ...meta, description }, content: args.content! });
        return undefined;
      }
      case 'agent_craft_delete':
        disk.delete(args.path!);
        return undefined;
      default:
        throw new Error(`unexpected invoke: ${cmd}`);
    }
  });
});

const renderView = () => {
  const onBack = vi.fn();
  const onParentKeyDown = vi.fn();
  const headingRef = createRef<HTMLHeadingElement>();
  render(
    // Like AgentEditor's root: focusable, and it leaves the view on Escape.
    <div onKeyDown={onParentKeyDown} tabIndex={-1}>
      <CraftView definitionId="def-1" headingRef={headingRef} onBack={onBack} context="Reviewer" />
    </div>
  );
  return { onBack, onParentKeyDown };
};

const fileList = () => screen.findByRole('list', { name: 'Craft files' });
const row = (path: RegExp) => screen.getByRole('button', { name: path });
const editor = () => screen.getByRole('textbox', { name: 'react/tests.md' });
const calls = (cmd: string) => mockInvoke.mock.calls.filter(([name]) => name === cmd);

const openTests = async () => {
  await fileList();
  await userEvent.click(row(/^react\/tests\.md/));
  return screen.findByRole('textbox', { name: 'react/tests.md' });
};

describe('CraftView', () => {
  it('lists each file with its description and date, and marks broken frontmatter', async () => {
    renderView();
    const items = within(await fileList()).getAllByRole('listitem');

    expect(items).toHaveLength(2);
    expect(items[0]).toHaveTextContent('react/tests.md');
    expect(items[0]).toHaveTextContent('2026-11-14');
    expect(items[0]).toHaveTextContent(TESTS.description!);
    expect(within(items[0]!).queryByText('Fix frontmatter')).toBeNull();
    expect(items[1]).toHaveTextContent('broken.md');
    expect(items[1]).toHaveTextContent('No description.');
    expect(within(items[1]!).getByText('Fix frontmatter')).toBeInTheDocument();
  });

  it('explains craft when the agent has written none', async () => {
    seed();
    renderView();
    expect(await screen.findByRole('status')).toHaveTextContent(
      'it writes down what it learns about doing its work, and it uses those notes in every workspace that assigns it'
    );
    expect(screen.queryByRole('list', { name: 'Craft files' })).toBeNull();
  });

  it('saves exactly what was typed, then lists again for the new description', async () => {
    renderView();
    const textarea = await openTests();
    expect(textarea).toHaveFocus();
    expect(textarea).toHaveValue(lesson(TESTS.description!, '2026-11-14'));
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save).toBeDisabled();

    const edited = lesson('Use when testing keys: fire them at document.activeElement.', '2026-11-15', 'New body\n');
    fireEvent.change(textarea, { target: { value: edited } });
    expect(screen.getByText('Unsaved changes')).toBeInTheDocument();
    await userEvent.click(save);

    await waitFor(() => expect(calls('agent_craft_list')).toHaveLength(2));
    expect(calls('agent_craft_write')).toEqual([
      ['agent_craft_write', { definitionId: 'def-1', path: 'react/tests.md', content: edited }],
    ]);
    expect(row(/^react\/tests\.md/)).toHaveTextContent('fire them at document.activeElement');
    expect(screen.queryByText('Unsaved changes')).toBeNull();
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(textarea).toHaveFocus();
  });

  it('asks before discarding unsaved changes, however the file is left', async () => {
    const { onBack, onParentKeyDown } = renderView();
    mockAsk.mockResolvedValue(false);
    await openTests();
    await userEvent.type(editor(), 'More');

    await userEvent.click(row(/^broken\.md/));
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    await userEvent.click(screen.getByRole('button', { name: 'Back to Reviewer' }));
    // A click on blank space must not hand Escape to the host.
    onParentKeyDown.mockClear();
    await userEvent.click(screen.getByText('What this agent has learnt, in every workspace'));
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(mockAsk).toHaveBeenCalledTimes(4));
    expect(mockAsk).toHaveBeenLastCalledWith(
      'Discard your changes to react/tests.md?',
      expect.objectContaining({ okLabel: 'Discard' })
    );
    expect(onParentKeyDown).not.toHaveBeenCalled();
    expect(editor()).toHaveValue(`${lesson(TESTS.description!, '2026-11-14')}More`);
    expect(onBack).not.toHaveBeenCalled();
    expect(calls('agent_craft_read')).toHaveLength(1);

    mockAsk.mockResolvedValue(true);
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    await waitFor(() => expect(screen.queryByRole('textbox')).toBeNull());
    expect(row(/^react\/tests\.md/)).toHaveFocus();
    expect(calls('agent_craft_write')).toHaveLength(0);
  });

  it('closes the open file on Escape instead of leaving the view', async () => {
    const { onParentKeyDown } = renderView();
    await openTests();

    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(onParentKeyDown).not.toHaveBeenCalled();
    expect(row(/^react\/tests\.md/)).toHaveFocus();

    // With nothing open, Escape is the host's: it leaves the view.
    await userEvent.keyboard('{Escape}');
    expect(onParentKeyDown).toHaveBeenCalledTimes(1);
  });

  it('asks once when Delete is clicked twice before the dialog appears', async () => {
    renderView();
    await openTests();
    let answer!: (value: boolean) => void;
    mockAsk.mockReturnValue(new Promise<boolean>((resolve) => { answer = resolve; }));
    const remove = screen.getByRole('button', { name: 'Delete' });

    await userEvent.dblClick(remove);
    expect(mockAsk).toHaveBeenCalledTimes(1);
    // Close the dialog: the one-dialog guard is app-wide and outlives this test.
    answer(false);
  });

  it('ignores Escape while a save is running', async () => {
    renderView();
    await openTests();
    await userEvent.type(editor(), 'More');
    mockInvoke.mockImplementationOnce(() => new Promise(() => {}));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeDisabled();

    editor().focus();
    await userEvent.keyboard('{Escape}');
    expect(mockAsk).not.toHaveBeenCalled();
    expect(editor()).toBeInTheDocument();
  });

  it('deletes only after the user confirms', async () => {
    renderView();
    mockAsk.mockResolvedValueOnce(false);
    await openTests();

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
    expect(mockAsk).toHaveBeenCalledWith(
      'Delete react/tests.md? The agent stops seeing this lesson. This cannot be undone.',
      expect.objectContaining({ okLabel: 'Delete' })
    );
    expect(calls('agent_craft_delete')).toHaveLength(0);
    expect(editor()).toBeInTheDocument();

    mockAsk.mockResolvedValueOnce(true);
    await userEvent.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(screen.queryByRole('textbox')).toBeNull());
    expect(calls('agent_craft_delete')).toEqual([['agent_craft_delete', { definitionId: 'def-1', path: 'react/tests.md' }]]);
    expect(within(await fileList()).getAllByRole('listitem')).toHaveLength(1);
    expect(row(/^broken\.md/)).toHaveFocus();
  });

  it('shows a failed read and opens nothing', async () => {
    renderView();
    await fileList();
    mockInvoke.mockImplementationOnce(() => Promise.reject('Failed to read react/tests.md: No such file or directory'));
    await userEvent.click(row(/^react\/tests\.md/));

    expect(await screen.findByRole('alert')).toHaveTextContent('Failed to read react/tests.md: No such file or directory');
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(screen.getByText('Choose a file to read or edit it.')).toBeInTheDocument();
  });

  it('keeps the file open when the delete fails', async () => {
    renderView();
    await openTests();
    mockAsk.mockResolvedValueOnce(true);
    mockInvoke.mockImplementationOnce(() => Promise.reject('Failed to delete react/tests.md: Permission denied'));
    await userEvent.click(screen.getByRole('button', { name: 'Delete' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Failed to delete react/tests.md: Permission denied');
    expect(editor()).toHaveFocus();
    expect(within(await fileList()).getAllByRole('listitem')).toHaveLength(2);
  });

  it('clears the error once a later action succeeds', async () => {
    renderView();
    await openTests();
    await userEvent.type(editor(), 'More');
    mockInvoke.mockImplementationOnce(() => Promise.reject('Failed to write react/tests.md: Disk full'));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Disk full');

    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.queryByText('Unsaved changes')).toBeNull());
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows the backend error as it is and keeps the edit', async () => {
    renderView();
    await openTests();
    mockInvoke.mockImplementationOnce(() => Promise.reject('Failed to write react/tests.md: Permission denied (os error 13)'));
    await userEvent.type(editor(), 'More');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /^Failed to write react\/tests\.md: Permission denied \(os error 13\)$/
    );
    expect(editor()).toHaveValue(`${lesson(TESTS.description!, '2026-11-14')}More`);
    expect(screen.getByText('Unsaved changes')).toBeInTheDocument();
  });

  it('shows a failed listing', async () => {
    mockInvoke.mockImplementationOnce(() => Promise.reject('Agent definition not found: def-1'));
    renderView();
    expect(await screen.findByRole('alert')).toHaveTextContent('Agent definition not found: def-1');
    expect(screen.queryByRole('status')).toBeNull();
  });
});
