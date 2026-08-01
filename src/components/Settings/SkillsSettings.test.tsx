import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

import SkillsSettings from './SkillsSettings';

const CATALOG = {
  sources: [
    { id: 'a', name: 'Bundled', enabled: true, managedKind: 'bundled', source: { kind: 'git', uri: 'https://example/skills.git' } },
    { id: 'b', name: 'Company', enabled: true, managedKind: null, source: { kind: 'local', path: '/srv/skills' } },
  ],
  skills: [
    { id: 'a/review', name: 'Code Review', description: 'Review a diff', sourceId: 'a', sourceName: 'Bundled', sourcePath: '/cache/a/review/SKILL.md', content: '' },
    { id: 'a/ledger', name: 'Work Ledger', description: 'Track long tasks', sourceId: 'a', sourceName: 'Bundled', sourcePath: '/cache/a/ledger/SKILL.md', content: '' },
    { id: 'b/deploy', name: 'Deploy', description: 'Ship to prod', sourceId: 'b', sourceName: 'Company', sourcePath: '/srv/skills/deploy/SKILL.md', content: '' },
  ],
  diagnostics: [],
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'skills_catalog') return Promise.resolve(CATALOG);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  });
});

describe('SkillsSettings', () => {
  it('lists each skill under its source instead of a separate flat list', async () => {
    render(<SkillsSettings />);
    await screen.findByText('Bundled');
    // Each source states its own count, and its skills are nested inside it.
    expect(screen.getByText('2 skills')).toBeInTheDocument();
    expect(screen.getByText('1 skill')).toBeInTheDocument();
    expect(screen.getByText('Code Review')).toBeInTheDocument();
    expect(screen.getByText('Deploy')).toBeInTheDocument();
    // The old parallel "Discovered Skills" section is gone.
    expect(screen.queryByText('Discovered Skills')).toBeNull();
  });

  it('keeps the add-source form out of the way until asked for', async () => {
    const user = userEvent.setup();
    render(<SkillsSettings />);
    await screen.findByText('Bundled');
    expect(screen.queryByPlaceholderText('/path/to/skills')).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Add source' }));
    expect(screen.getByPlaceholderText('/path/to/skills')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByPlaceholderText('/path/to/skills')).toBeNull();
  });

  it('searches across every source', async () => {
    const user = userEvent.setup();
    render(<SkillsSettings />);
    await screen.findByText('Bundled');

    await user.type(screen.getByLabelText('Search skills'), 'ship');
    expect(screen.getByText('Deploy')).toBeInTheDocument();
    expect(screen.queryByText('Code Review')).toBeNull();
    expect(screen.queryByText('Bundled')).toBeNull();
  });

  it('counts what the search actually shows, not the whole catalog', async () => {
    const user = userEvent.setup();
    render(<SkillsSettings />);
    await screen.findByText('Bundled');
    expect(screen.getByText('3 skills')).toBeInTheDocument();

    await user.type(screen.getByLabelText('Search skills'), 'ledger');
    // Header tally and the source's own tally both narrow to the match.
    expect(screen.getByText('1 of 3 skills')).toBeInTheDocument();
    expect(screen.getByText('1 of 2 skills')).toBeInTheDocument();
    expect(screen.queryByText('2 skills')).toBeNull();
  });

  it('collapses a source', async () => {
    const user = userEvent.setup();
    render(<SkillsSettings />);
    await screen.findByText('Bundled');

    await user.click(screen.getByRole('button', { expanded: true, name: /Bundled/ }));
    await waitFor(() => expect(screen.queryByText('Code Review')).toBeNull());
    expect(screen.getByText('Deploy')).toBeInTheDocument();
  });

  it('points at the add-source action when nothing is configured', async () => {
    mockInvoke.mockImplementation(() => Promise.resolve({ sources: [], skills: [], diagnostics: [] }));
    render(<SkillsSettings />);
    expect(await screen.findByText(/No skill sources configured/)).toBeInTheDocument();
  });
});
