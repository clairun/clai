import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { useState } from 'react';

// Only the Agents tab is under test; the other panes are heavy modules this
// shell merely switches between.
vi.mock('./AssistantProviderSettings', () => ({ default: () => <div /> }));
vi.mock('./McpServersSettings', () => ({ default: () => <div /> }));
vi.mock('./SkillsSettings', () => ({ default: () => <div /> }));
vi.mock('./AppearanceSettings', () => ({ default: () => <div /> }));
vi.mock('./ApplicationsSettings', () => ({ default: () => <div /> }));
vi.mock('./AboutSettings', () => ({ default: () => <div /> }));
vi.mock('./AgentLibrarySettings', () => ({
  // Mirrors the real library: the initial id is read once per mount, so a
  // second deep link only lands if this shell remounts it.
  default: ({ initialAgentDefinitionId }: { initialAgentDefinitionId?: string | null }) => {
    const [consumed] = useState(initialAgentDefinitionId ?? 'gallery');
    return <div data-testid="library">{consumed}</div>;
  },
}));

import SettingsModal, { TABS } from './SettingsModal';

const modal = (props: { isOpen: boolean; initialAgentDefinitionId?: string | null }) => (
  <SettingsModal
    isOpen={props.isOpen}
    onClose={vi.fn()}
    initialTab={TABS.AGENTS}
    initialAgentDefinitionId={props.initialAgentDefinitionId}
  />
);

describe('SettingsModal agent deep link', () => {
  it('hands the agents tab the definition a deep link named', () => {
    render(modal({ isOpen: true, initialAgentDefinitionId: 'def-1' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');
  });

  // Every deep link comes from a control this modal's overlay covers, so the
  // next one can only arrive after a close — which unmounts the tab and lets
  // the library read the new id. No remount key needed to force it.
  it('lands a later deep link on a different agent once it has been closed', () => {
    const { rerender } = render(modal({ isOpen: true, initialAgentDefinitionId: 'def-1' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');

    rerender(modal({ isOpen: false, initialAgentDefinitionId: null }));
    expect(screen.queryByTestId('library')).toBeNull();

    rerender(modal({ isOpen: true, initialAgentDefinitionId: 'def-2' }));
    expect(screen.getByTestId('library')).toHaveTextContent('def-2');
  });

  it('opens the plain gallery when no agent was named', () => {
    render(modal({ isOpen: true }));
    expect(screen.getByTestId('library')).toHaveTextContent('gallery');
  });
});
