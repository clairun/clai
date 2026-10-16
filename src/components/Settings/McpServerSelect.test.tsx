import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import McpServerSelect, { summarizeNames } from './McpServerSelect';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';

const SERVERS = [
  { id: 'gh', name: 'github' },
  { id: 'nd', name: 'netdata', description: 'Metrics' },
  { id: 'fs', name: 'files' },
];

const Host = ({ initial = ['gh'], onChange = vi.fn(), onModalEscape = vi.fn(), servers = SERVERS }: {
  initial?: string[];
  onChange?: (ids: string[]) => void;
  onModalEscape?: () => void;
  servers?: typeof SERVERS;
}) => {
  const [ids, setIds] = useState(initial);
  // Stands in for the settings window underneath, which also takes Escape.
  useOverlayLayer(true, onModalEscape);
  return (
    <>
      <span id="mcp-label">MCP servers</span>
      <McpServerSelect
        servers={servers}
        selectedIds={ids}
        onChange={(next) => { setIds(next); onChange(next); }}
        labelId="mcp-label"
      />
      <button type="button">Next control</button>
    </>
  );
};

describe('summarizeNames', () => {
  it('names the first two and counts the rest', () => {
    expect(summarizeNames([])).toBe('None');
    expect(summarizeNames(['a', 'b'])).toBe('a, b');
    expect(summarizeNames(['a', 'b', 'c', 'd'])).toBe('a, b +2');
  });
});

describe('McpServerSelect', () => {
  it('closes on Escape without the window underneath seeing it, and returns focus', async () => {
    const onModalEscape = vi.fn();
    render(<Host onModalEscape={onModalEscape} />);
    const trigger = screen.getByRole('button', { name: 'MCP servers github' });

    await userEvent.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    await userEvent.keyboard('{Escape}');

    expect(screen.queryByRole('group', { name: 'MCP servers' })).toBeNull();
    expect(onModalEscape).not.toHaveBeenCalled();
    expect(trigger).toHaveFocus();
  });

  it('moves between servers with the arrow keys', async () => {
    render(<Host />);
    await userEvent.click(screen.getByRole('button', { name: /^MCP servers/ }));

    await userEvent.keyboard('{ArrowDown}');
    expect(screen.getByRole('checkbox', { name: /netdata/ })).toHaveFocus();
    await userEvent.keyboard('{ArrowUp}{ArrowUp}');
    expect(screen.getByRole('checkbox', { name: 'files' })).toHaveFocus();
  });

  it('turns every listed server off but keeps ids it does not know about', async () => {
    const onChange = vi.fn();
    render(<Host initial={['gh', 'gone']} onChange={onChange} />);
    await userEvent.click(screen.getByRole('button', { name: /^MCP servers/ }));

    await userEvent.click(screen.getByRole('button', { name: 'Turn all off' }));

    expect(onChange).toHaveBeenLastCalledWith(['gone']);
    expect(screen.getByRole('button', { name: 'MCP servers None' })).toBeInTheDocument();
  });

  it('keeps Tab order trigger → popover → next control, closing once focus moves past it', async () => {
    render(<Host />);
    const trigger = screen.getByRole('button', { name: /^MCP servers/ });
    await userEvent.click(trigger);
    expect(screen.getByRole('checkbox', { name: 'github' })).toHaveFocus();

    // Back past the header's Turn all off reaches the trigger, still open.
    await userEvent.tab({ shift: true });
    await userEvent.tab({ shift: true });
    expect(trigger).toHaveFocus();
    expect(screen.getByRole('group', { name: 'MCP servers' })).toBeInTheDocument();

    screen.getByRole('button', { name: 'Manage servers…' }).focus();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'Next control' })).toHaveFocus();
    expect(screen.queryByRole('group', { name: 'MCP servers' })).toBeNull();
  });

  it('closes on a click outside', async () => {
    render(<Host />);
    await userEvent.click(screen.getByRole('button', { name: /^MCP servers/ }));

    await userEvent.click(document.body);

    expect(screen.queryByRole('group', { name: 'MCP servers' })).toBeNull();
  });

  it('is disabled and says so when no server is configured', () => {
    render(<Host servers={[]} />);

    expect(screen.getByRole('button', { name: 'MCP servers None configured' })).toBeDisabled();
  });
});
