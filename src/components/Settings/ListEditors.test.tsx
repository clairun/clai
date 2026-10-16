import { describe, expect, it, vi } from 'vitest';
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
});
