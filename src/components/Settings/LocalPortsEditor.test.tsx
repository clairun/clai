import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ComponentProps } from 'react';
import type { LocalPortGrant } from '../../generated/bindings';
import { LocalPortsEditor } from './LocalPortsEditor';

const SSH: LocalPortGrant = { kind: 'program', port: 5432, program: '/usr/bin/ssh' };
const POSTGRES: LocalPortGrant = { kind: 'user', port: 5433, uid: 120, user: 'postgres' };
const UNNAMED: LocalPortGrant = { kind: 'user', port: 6379, uid: 999 };

const renderEditor = (props: Partial<ComponentProps<typeof LocalPortsEditor>> = {}) => {
  const onChange = vi.fn();
  render(<LocalPortsEditor grants={[SSH, POSTGRES, UNNAMED]} onChange={onChange} {...props} />);
  return onChange;
};

const removeButtons = () => screen.getAllByRole('button', { name: /^Remove / });

describe('LocalPortsEditor', () => {
  it('lists each port with the program or user that must serve it', () => {
    renderEditor();
    expect(screen.getByText('Local ports · 3')).toBeInTheDocument();
    const rows = within(screen.getByRole('list', { name: 'Local ports' })).getAllByRole('listitem');
    expect(rows.map((row) => row.textContent)).toEqual([
      'localhost:5432ssh×',
      'localhost:5433any program of user postgres×',
      'localhost:6379any program of uid 999×',
    ]);
    expect(screen.getByText('any program of user postgres')).toHaveAttribute('title', 'uid 120');
    expect(screen.getByText('ssh')).toHaveAttribute('title', '/usr/bin/ssh');
  });

  it('names each row on its remove button, and saves the list without the row removed', async () => {
    const onChange = renderEditor();
    expect(removeButtons().map((button) => button.getAttribute('aria-label'))).toEqual([
      'Remove localhost:5432 (/usr/bin/ssh)',
      'Remove localhost:5433 (any program of user postgres)',
      'Remove localhost:6379 (any program of uid 999)',
    ]);
    await userEvent.click(screen.getByRole('button', { name: 'Remove localhost:5433 (any program of user postgres)' }));
    expect(onChange).toHaveBeenCalledExactlyOnceWith([SSH, UNNAMED]);
  });

  it('removes one copy of a grant a hand edit repeated', async () => {
    const onChange = renderEditor({ grants: [SSH, { ...SSH }] });
    await userEvent.click(removeButtons()[1]!);
    expect(onChange).toHaveBeenCalledExactlyOnceWith([SSH]);
  });

  it('says how a port gets on the list when there is none, without a deny note', () => {
    renderEditor({ grants: [], deniedBy: 'localhost' });
    expect(screen.queryByText(/Denied hosts/)).toBeNull();
    expect(screen.getByText('Local ports · 0')).toBeInTheDocument();
    expect(screen.getByText('None. Approve an agent’s request to add one.')).toBeInTheDocument();
    expect(screen.queryByRole('list')).toBeNull();
  });

  it('notes that a denied name of this computer refuses the ports, and still lets them be removed', async () => {
    const onChange = renderEditor({ deniedBy: 'localhost' });
    expect(screen.getByText('Denied hosts include localhost, so these ports are refused.')).toBeInTheDocument();
    await userEvent.click(removeButtons()[0]!);
    expect(onChange).toHaveBeenCalledExactlyOnceWith([POSTGRES, UNNAMED]);
  });

  it('adds no note when nothing denied refuses the ports', () => {
    renderEditor();
    expect(screen.queryByText(/Denied hosts/)).toBeNull();
  });

  it('disables removing while the form is busy', () => {
    renderEditor({ disabled: true });
    for (const button of removeButtons()) expect(button).toBeDisabled();
  });
});
