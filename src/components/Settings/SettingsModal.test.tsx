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

describe('SettingsModal agent deep link', () => {
  it('hands the agents tab the definition a deep link named', () => {
    const { rerender } = render(
      <SettingsModal
        isOpen
        onClose={vi.fn()}
        initialTab={TABS.AGENTS}
        initialAgentDefinitionId="def-1"
      />
    );
    expect(screen.getByTestId('library')).toHaveTextContent('def-1');

    // A second deep link while the tab is already up must not be swallowed by
    // the library's mounted state.
    rerender(
      <SettingsModal
        isOpen
        onClose={vi.fn()}
        initialTab={TABS.AGENTS}
        initialAgentDefinitionId="def-2"
      />
    );
    expect(screen.getByTestId('library')).toHaveTextContent('def-2');
  });

  it('opens the plain gallery when no agent was named', () => {
    render(<SettingsModal isOpen onClose={vi.fn()} initialTab={TABS.AGENTS} />);
    expect(screen.getByTestId('library')).toHaveTextContent('gallery');
  });
});
