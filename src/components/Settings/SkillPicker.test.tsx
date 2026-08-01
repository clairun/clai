import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import SkillPicker, { type SkillOption } from './SkillPicker';

const SKILLS: SkillOption[] = [
  { id: 'a/review', name: 'Code Review', description: 'Review a diff', sourceId: 'a', sourceName: 'Bundled' },
  { id: 'a/ledger', name: 'Work Ledger', description: 'Track long tasks', sourceId: 'a', sourceName: 'Bundled' },
  { id: 'b/deploy', name: 'Deploy', description: 'Ship to prod', sourceId: 'b', sourceName: 'Company' },
];

/** Renders with selection held in a closure so onChange round-trips. */
const setup = (initial: string[] = []) => {
  const onChange = vi.fn();
  const view = render(
    <SkillPicker skills={SKILLS} selectedIds={initial} onChange={onChange} />
  );
  return { onChange, view };
};

describe('SkillPicker', () => {
  it('groups skills under their source with a selected count', () => {
    setup(['a/review']);
    expect(screen.getByText('Bundled')).toBeInTheDocument();
    expect(screen.getByText('Company')).toBeInTheDocument();
    expect(screen.getByText('1/2')).toBeInTheDocument();
    expect(screen.getByText('0/1')).toBeInTheDocument();
    expect(screen.getByText('1 of 3 selected')).toBeInTheDocument();
  });

  it('filters by name and by description', async () => {
    const user = userEvent.setup();
    setup();
    const search = screen.getByLabelText('Search skills');

    await user.type(search, 'ledger');
    expect(screen.getByText('Work Ledger')).toBeInTheDocument();
    expect(screen.queryByText('Code Review')).toBeNull();
    expect(screen.queryByText('Company')).toBeNull();

    await user.clear(search);
    await user.type(search, 'ship to prod');
    expect(screen.getByText('Deploy')).toBeInTheDocument();
    expect(screen.queryByText('Work Ledger')).toBeNull();
  });

  it('reports no matches without hiding the search box', async () => {
    const user = userEvent.setup();
    setup();
    await user.type(screen.getByLabelText('Search skills'), 'zzz');
    expect(screen.getByText(/No skills match/)).toBeInTheDocument();
    expect(screen.getByLabelText('Search skills')).toBeInTheDocument();
  });

  it('selects every skill in a source in one action', async () => {
    const user = userEvent.setup();
    const { onChange } = setup();
    const bundled = screen.getByText('Bundled').closest('section') as HTMLElement;
    await user.click(within(bundled).getByRole('button', { name: 'Select all' }));
    expect(onChange).toHaveBeenCalledWith(['a/review', 'a/ledger']);
  });

  it('clears a source without touching other sources', async () => {
    const user = userEvent.setup();
    const { onChange } = setup(['a/review', 'a/ledger', 'b/deploy']);
    const bundled = screen.getByText('Bundled').closest('section') as HTMLElement;
    await user.click(within(bundled).getByRole('button', { name: 'Clear' }));
    expect(onChange).toHaveBeenCalledWith(['b/deploy']);
  });

  it('toggles a single skill by checkbox', async () => {
    const user = userEvent.setup();
    const { onChange } = setup([]);
    await user.click(screen.getByRole('checkbox', { name: /Deploy/ }));
    expect(onChange).toHaveBeenCalledWith(['b/deploy']);
  });

  it('removes a selected skill from its chip', async () => {
    const user = userEvent.setup();
    const { onChange } = setup(['a/review', 'b/deploy']);
    await user.click(screen.getByRole('button', { name: 'Remove Deploy' }));
    expect(onChange).toHaveBeenCalledWith(['a/review']);
  });

  it('clears the whole selection', async () => {
    const user = userEvent.setup();
    const { onChange } = setup(['a/review', 'b/deploy']);
    await user.click(screen.getByRole('button', { name: 'Clear all' }));
    expect(onChange).toHaveBeenCalledWith([]);
  });

  it('collapses a source and hides only its skills', async () => {
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole('button', { expanded: true, name: /Bundled/ }));
    expect(screen.queryByText('Code Review')).toBeNull();
    expect(screen.getByText('Deploy')).toBeInTheDocument();
  });

  it('keeps groups open while a search is active', async () => {
    const user = userEvent.setup();
    setup();
    await user.click(screen.getByRole('button', { expanded: true, name: /Bundled/ }));
    expect(screen.queryByText('Code Review')).toBeNull();
    await user.type(screen.getByLabelText('Search skills'), 'review');
    expect(screen.getByText('Code Review')).toBeInTheDocument();
  });

  it('tells the user where skills come from when there are none', () => {
    render(<SkillPicker skills={[]} selectedIds={[]} onChange={vi.fn()} />);
    expect(screen.getByText(/Add a skill source in app settings/)).toBeInTheDocument();
  });

  it('disables every control when disabled', () => {
    render(<SkillPicker skills={SKILLS} selectedIds={['a/review']} onChange={vi.fn()} disabled />);
    expect(screen.getByLabelText('Search skills')).toBeDisabled();
    expect(screen.getByRole('checkbox', { name: /Deploy/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Clear all' })).toBeDisabled();
  });
});
