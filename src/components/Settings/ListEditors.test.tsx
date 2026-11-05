import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { PathGrantEditor, PrefixListEditor } from './ListEditors';

describe('PathGrantEditor', () => {
  const make = (path: string, access: string) => ({ path, access });

  it('adds a path with the chosen access on Enter, and ignores a duplicate', async () => {
    const onChange = vi.fn();
    render(<PathGrantEditor grants={[{ path: '/a', access: 'read_only' }]} onChange={onChange} make={make} />);
    const input = screen.getByRole('textbox', { name: 'New path for path grants' });

    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Access for the new path' }), 'read_write');
    await userEvent.type(input, ' /b {Enter}');
    expect(onChange).toHaveBeenCalledWith([{ path: '/a', access: 'read_only' }, { path: '/b', access: 'read_write' }]);
    expect(input).toHaveValue('');

    onChange.mockClear();
    await userEvent.type(input, '/a{Enter}');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('removes a grant and spells out its access', async () => {
    const onChange = vi.fn();
    render(<PathGrantEditor grants={[{ path: '/a', access: 'read_write' }]} onChange={onChange} make={make} />);

    expect(within(screen.getByRole('list', { name: 'Path grants' })).getByText('Read & write')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Remove grant for /a' }));

    expect(onChange).toHaveBeenCalledWith([]);
  });

  it('says when nothing is granted', () => {
    render(<PathGrantEditor grants={[]} onChange={vi.fn()} make={make} />);

    expect(screen.getByText('No paths granted.')).toBeInTheDocument();
  });
});

describe('PrefixListEditor', () => {
  afterEach(() => { vi.restoreAllMocks(); });

  it('adds a trimmed prefix once and removes it again', async () => {
    const onChange = vi.fn();
    const { rerender } = render(<PrefixListEditor items={[]} onChange={onChange} label="Blocked" placeholder="rm" />);

    await userEvent.type(screen.getByRole('textbox', { name: 'New entry for blocked' }), '  sudo ');
    await userEvent.click(screen.getByRole('button', { name: 'Add' }));
    expect(onChange).toHaveBeenCalledWith(['sudo']);

    rerender(<PrefixListEditor items={['sudo']} onChange={onChange} label="Blocked" placeholder="rm" />);
    await userEvent.click(screen.getByRole('button', { name: 'Remove sudo' }));
    expect(onChange).toHaveBeenLastCalledWith([]);
  });

  it('collapses the spaces inside an entry', async () => {
    const onChange = vi.fn();
    render(<PrefixListEditor items={[]} onChange={onChange} label="Ask first" placeholder="git push" />);

    await userEvent.type(screen.getByRole('textbox', { name: 'New entry for ask first' }), ' git   push {Enter}');
    expect(onChange).toHaveBeenCalledWith(['git push']);
  });

  it('ignores a duplicate, including one that differs only in spacing', async () => {
    const onChange = vi.fn();
    render(<PrefixListEditor items={['git push']} onChange={onChange} label="Ask first" placeholder="git push" />);
    const input = screen.getByRole('textbox', { name: 'New entry for ask first' });

    await userEvent.type(input, 'git push{Enter}');
    expect(screen.getByRole('alert')).toHaveTextContent('git push is already in the list.');
    expect(input).toHaveAccessibleDescription('git push is already in the list.');
    await userEvent.type(input, 'git    push{Enter}');
    expect(onChange).not.toHaveBeenCalled();
    expect(input).toHaveValue('');
  });

  it('does nothing on Enter in an empty field', async () => {
    const onChange = vi.fn();
    render(<PrefixListEditor items={[]} onChange={onChange} label="Hosts" placeholder="x" parse={() => ({ error: 'bad' })} />);

    await userEvent.type(screen.getByRole('textbox', { name: 'New entry for hosts' }), '   {Enter}');
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('scrolls a new entry into view', async () => {
    const scrollIntoView = vi.spyOn(Element.prototype, 'scrollIntoView');
    const items = ['a', 'b', 'c'];
    const onChange = vi.fn();
    const { rerender } = render(<PrefixListEditor items={items} onChange={onChange} label="Hosts" placeholder="x" />);

    await userEvent.type(screen.getByRole('textbox', { name: 'New entry for hosts' }), 'd{Enter}');
    rerender(<PrefixListEditor items={onChange.mock.calls[0]![0]} onChange={onChange} label="Hosts" placeholder="x" />);

    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView.mock.contexts[0]).toHaveTextContent('d');
  });
});
