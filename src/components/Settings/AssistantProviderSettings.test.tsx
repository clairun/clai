import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const client = vi.hoisted(() => ({
  listProviderConnections: vi.fn().mockResolvedValue([]),
  listAvailableProviderAdapters: vi.fn().mockResolvedValue([]),
  listProviderCatalog: vi.fn().mockResolvedValue([
    {
      id: 'openai',
      protocolId: 'openai',
      displayName: 'OpenAI',
      description: 'Hosted models',
      category: 'hosted',
      requiresApiKey: true,
      defaultBaseUrl: '',
      baseUrlLocked: false,
      logoAsset: 'provider-catalog/openai.svg',
      curatedModels: [],
      docsUrl: null,
      extraHeaders: [],
      modelsEndpointStyle: 'openai',
      capabilities: null,
    },
  ]),
  getProviderSecretHint: vi.fn().mockResolvedValue(null),
}));
vi.mock('../../assistant', () => ({ assistantClient: client }));
// The other tabs are heavy modules this test never opens; the provider pane
// and the modal shell around it are both real, because their overlays stack.
vi.mock('./McpServersSettings', () => ({ default: () => <div /> }));
vi.mock('./SkillsSettings', () => ({ default: () => <div /> }));
vi.mock('./AppearanceSettings', () => ({ default: () => <div /> }));
vi.mock('./ApplicationsSettings', () => ({ default: () => <div /> }));
vi.mock('./AboutSettings', () => ({ default: () => <div /> }));
vi.mock('./AgentLibrarySettings', () => ({ default: () => <div /> }));

import SettingsModal, { TABS } from './SettingsModal';

const openSettings = () => {
  const onClose = vi.fn();
  render(<SettingsModal isOpen onClose={onClose} initialTab={TABS.PROVIDER} />);
  return { onClose };
};

describe('provider overlays over the Settings modal', () => {
  it('gives Escape to the provider picker, not to the modal under it', async () => {
    const user = userEvent.setup();
    const { onClose } = openSettings();

    await user.click(await screen.findByRole('button', { name: '+ Add Connection' }));
    expect(screen.getByRole('heading', { name: 'Choose a provider' })).toBeInTheDocument();

    await user.keyboard('{Escape}');
    // The picker goes; the Settings modal it was opened from stays.
    expect(screen.queryByRole('heading', { name: 'Choose a provider' })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('heading', { name: 'Settings' })).toBeInTheDocument();

    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('gives Escape to the connection form, not to the modal under it', async () => {
    const user = userEvent.setup();
    const { onClose } = openSettings();

    await user.click(await screen.findByRole('button', { name: '+ Add Connection' }));
    await user.click(await screen.findByText('OpenAI'));
    expect(screen.getByRole('heading', { name: /Add OpenAI/ })).toBeInTheDocument();

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('heading', { name: /Add OpenAI/ })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('keeps the page locked while the picker closes over the modal', async () => {
    const user = userEvent.setup();
    openSettings();

    await user.click(await screen.findByRole('button', { name: '+ Add Connection' }));
    await user.keyboard('{Escape}');

    expect(document.body.style.overflow).toBe('hidden');
  });
});
