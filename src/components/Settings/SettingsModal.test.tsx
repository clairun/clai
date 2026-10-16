import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';

// The Agents tab is a stub that shows its deep link, and Appearance is real
// (it calls nothing); the other panes are heavy modules this shell merely
// switches between.
vi.mock('./AssistantProviderSettings', () => ({ default: () => <div /> }));
vi.mock('./McpServersSettings', () => ({ default: () => <div /> }));
vi.mock('./SkillsSettings', () => ({ default: () => <div /> }));
vi.mock('./ApplicationsSettings', () => ({ default: () => <div /> }));
vi.mock('./AboutSettings', () => ({ default: () => <div /> }));
vi.mock('./AgentLibrarySettings', () => ({
  // Mirrors the real library: the initial id is read once per mount, so a
  // second deep link only lands if this shell remounts it.
  default: function LibraryStub({
    initialAgentDefinitionId,
  }: {
    initialAgentDefinitionId?: string | null;
  }) {
    const [consumed] = useState(initialAgentDefinitionId ?? 'gallery');
    return <div data-testid="library">{consumed}</div>;
  },
}));

import SettingsModal, { TABS } from './SettingsModal';

type Tab = (typeof TABS)[keyof typeof TABS];

const modal = (props: { isOpen: boolean; initialAgentDefinitionId?: string | null; initialTab?: Tab }) => (
  <SettingsModal
    isOpen={props.isOpen}
    onClose={vi.fn()}
    initialTab={props.initialTab ?? TABS.AGENTS}
    initialAgentDefinitionId={props.initialAgentDefinitionId}
  />
);

describe('SettingsModal agent deep link', () => {
  it('hands the agents tab the definition a deep link named', () => {
    render(modal({ isOpen: true, initialAgentDefinitionId: 'def-1' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');
  });

  // Every deep link comes from a control this modal's overlay covers, so the
  // next one can only arrive after a close, which unmounts the window.
  it('lands a later deep link on a different agent once it has been closed', () => {
    const { rerender } = render(modal({ isOpen: true, initialAgentDefinitionId: 'def-1' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');

    rerender(modal({ isOpen: false, initialAgentDefinitionId: null }));
    expect(screen.queryByTestId('library')).toBeNull();

    rerender(modal({ isOpen: true, initialAgentDefinitionId: 'def-2' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-2');
  });

  it('forgets the deep link once the user has left the agents tab', async () => {
    const user = userEvent.setup();
    render(modal({ isOpen: true, initialAgentDefinitionId: 'def-1' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');

    // Tabs are a switch: leaving unmounts the library, returning mounts a new
    // one. It must come back to the gallery, not to the editor the user just
    // navigated away from.
    await user.click(screen.getByRole('button', { name: 'Skills' }));
    await user.click(screen.getByRole('button', { name: 'Agents' }));
    expect(screen.getByTestId('library')).toHaveTextContent('gallery');
  });

  it('opens the plain gallery when no agent was named', () => {
    render(modal({ isOpen: true }));
    expect(screen.getByTestId('library')).toHaveTextContent('gallery');
  });
});

describe('SettingsModal window', () => {
  it('reopens on the tab asked for, not the one it was closed on', () => {
    const { rerender } = render(modal({ isOpen: true, initialTab: TABS.AGENTS }));
    expect(screen.getByRole('heading', { level: 2 })).toHaveTextContent('Agents');

    rerender(modal({ isOpen: false, initialTab: TABS.AGENTS }));
    rerender(modal({ isOpen: true, initialTab: TABS.APPEARANCE }));
    expect(screen.getByRole('heading', { level: 2 })).toHaveTextContent('Appearance');
    expect(screen.getByRole('button', { name: 'Appearance' })).toHaveAttribute('aria-current', 'page');
    expect(screen.queryByTestId('library')).toBeNull();
  });

  it('moves focus into the dialog, onto the page it opened on', () => {
    render(modal({ isOpen: true, initialTab: TABS.APPEARANCE }));
    expect(screen.getByRole('button', { name: 'Appearance' })).toHaveFocus();
  });

  it('names the page once: in the window header, not again in the page', () => {
    render(modal({ isOpen: true, initialTab: TABS.APPEARANCE }));
    expect(screen.getAllByRole('heading', { name: 'Appearance' })).toHaveLength(1);
  });
});
