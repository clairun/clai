import { describe, expect, it, vi } from 'vitest';
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';
import { SkillSourceView, SkillSources, groupSkills, type SkillOption } from './SkillPicker';

const SKILLS: SkillOption[] = [
  { id: 'xlsx', name: 'xlsx', description: 'Spreadsheets', sourceId: 'a', sourceName: 'anthropic-skills' },
  { id: 'pdf', name: 'pdf', description: 'Read PDFs', sourceId: 'a', sourceName: 'anthropic-skills' },
  { id: 'loose', name: 'loose' },
  { id: 'review', name: 'code-review', description: 'Review a change', sourceId: 'c', sourceName: 'clai-skills' },
];

describe('groupSkills', () => {
  it('groups by source, sorted by name, with source-less skills under Other', () => {
    expect(groupSkills(SKILLS).map((g) => [g.name, g.skills.map((s) => s.id)])).toEqual([
      ['anthropic-skills', ['pdf', 'xlsx']],
      ['clai-skills', ['review']],
      ['Other', ['loose']],
    ]);
  });
});

describe('SkillSources', () => {
  it('opens the source a row stands for', async () => {
    const onOpen = vi.fn();
    render(<SkillSources groups={groupSkills(SKILLS)} selectedIds={['pdf']} onChange={vi.fn()} onOpen={onOpen} />);

    await userEvent.click(screen.getByRole('button', { name: 'clai-skills, 0 of 1 selected' }));

    expect(onOpen).toHaveBeenCalledWith('c');
    expect(screen.getByRole('button', { name: 'anthropic-skills, 1 of 2 selected' })).toBeInTheDocument();
  });

  it('toggles a skill straight from the search results, matching descriptions too', async () => {
    const onChange = vi.fn();
    render(<SkillSources groups={groupSkills(SKILLS)} selectedIds={['pdf']} onChange={onChange} onOpen={vi.fn()} />);

    await userEvent.type(screen.getByRole('searchbox', { name: 'Search skills' }), 'spread');
    await userEvent.click(screen.getByRole('checkbox', { name: 'xlsx' }));

    expect(onChange).toHaveBeenCalledWith(['pdf', 'xlsx']);
  });
});

const renderSource = (selectedIds: string[] = [], disabled = false) => {
  const onChange = vi.fn();
  const onBack = vi.fn();
  const group = groupSkills(SKILLS)[0]!;
  render(
    <SkillSourceView
      group={group}
      selectedIds={selectedIds}
      onChange={onChange}
      onBack={onBack}
      headingRef={createRef()}
      context="Main"
      disabled={disabled}
    />
  );
  return { onChange, onBack };
};

describe('SkillSourceView', () => {
  it('marks selected tiles and keeps ids from other sources when toggling', async () => {
    const { onChange } = renderSource(['review', 'pdf']);

    expect(screen.getByRole('checkbox', { name: 'pdf' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'xlsx' })).not.toBeChecked();
    await userEvent.click(screen.getByRole('checkbox', { name: 'pdf' }));

    expect(onChange).toHaveBeenCalledWith(['review']);
  });

  it('clears only this source when part of it is selected', async () => {
    const { onChange } = renderSource(['review', 'pdf']);

    await userEvent.click(screen.getByRole('button', { name: 'Clear' }));

    expect(onChange).toHaveBeenCalledWith(['review']);
  });

  it('describes the skill under focus in the strip, since tiles show names only', async () => {
    renderSource();
    expect(screen.getByText(/Point at a skill/)).toBeInTheDocument();

    act(() => screen.getByRole('checkbox', { name: 'xlsx' }).focus());

    expect(screen.getByText('Spreadsheets')).toBeInTheDocument();
  });

  it('filters within the source and says when nothing matches', async () => {
    renderSource();

    await userEvent.type(screen.getByRole('searchbox', { name: 'Filter anthropic-skills' }), 'pdf');
    expect(within(screen.getByRole('list', { name: 'Skills in anthropic-skills' })).getAllByRole('checkbox')).toHaveLength(1);

    await userEvent.type(screen.getByRole('searchbox', { name: 'Filter anthropic-skills' }), 'zzz');
    expect(screen.getByRole('status')).toHaveTextContent('No skills in anthropic-skills match');
  });

  it('goes back through its crumb', async () => {
    const { onBack } = renderSource();

    await userEvent.click(screen.getByRole('button', { name: 'Back to Main' }));

    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it('disables every selection control when disabled', () => {
    renderSource([], true);

    for (const box of screen.getAllByRole('checkbox')) expect(box).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Select all' })).toBeDisabled();
  });
});
