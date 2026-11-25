import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ProviderCatalogEntry } from '../../generated/bindings';

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
  deleteProviderConnection: vi.fn(),
  listProviderDescriptorModels: vi.fn().mockResolvedValue([]),
  createProviderConnection: vi.fn(),
  updateProviderConnection: vi.fn(),
}));
vi.mock('../../assistant', () => ({ assistantClient: client }));
const mockAsk = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/plugin-dialog', () => ({ ask: mockAsk }));
vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn().mockResolvedValue(undefined) }));
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
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeInTheDocument();

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

// Deleting a connection also wipes its API key from the keychain, so the
// click must wait for the user's answer.
describe('deleting a provider connection', () => {
  const WORK = {
    id: 'conn-1',
    name: 'Work OpenAI',
    protocolId: 'openai',
    providerId: 'openai',
    authMode: 'developer_api_key',
    baseUrl: null,
    secretRef: 'conn-1',
    modelId: 'gpt-5',
    accountLabel: null,
    enabled: true,
  };

  beforeEach(() => {
    let connections = [WORK];
    client.listProviderConnections.mockImplementation(async () => connections);
    client.deleteProviderConnection.mockReset().mockImplementation(async (id: string) => {
      connections = connections.filter((connection) => connection.id !== id);
    });
    mockAsk.mockReset();
  });

  it('keeps the connection when the user cancels', async () => {
    const user = userEvent.setup();
    mockAsk.mockResolvedValue(false);
    openSettings();

    await user.click(await screen.findByRole('button', { name: 'Delete' }));

    await waitFor(() => expect(mockAsk).toHaveBeenCalledTimes(1));
    expect(mockAsk).toHaveBeenCalledWith(
      expect.stringContaining('"Work OpenAI"'),
      expect.objectContaining({ kind: 'warning', okLabel: 'Delete' })
    );
    expect(client.deleteProviderConnection).not.toHaveBeenCalled();
    expect(screen.getByText('Work OpenAI')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Delete' })).toBeEnabled();
  });

  it('deletes the connection once when the user confirms', async () => {
    const user = userEvent.setup();
    mockAsk.mockResolvedValue(true);
    openSettings();

    await user.click(await screen.findByRole('button', { name: 'Delete' }));

    expect(await screen.findByText('Connection deleted.')).toBeInTheDocument();
    expect(client.deleteProviderConnection).toHaveBeenCalledTimes(1);
    expect(client.deleteProviderConnection).toHaveBeenCalledWith('conn-1');
    expect(screen.queryByText('Work OpenAI')).toBeNull();
  });
});

// CLI connections take env vars for the CLI process (e.g. CLAUDE_CONFIG_DIR
// to pick an account); API connections spawn nothing, and the backend rejects
// env on them.
describe('provider connection environment variables', () => {
  const CLAUDE_CODE = {
    id: 'claude-code',
    displayName: 'Claude Code',
    protocolFamily: 'anthropic',
    supportedAuthModes: ['subscription_login'],
    configurableBaseUrl: true,
    isCliBacked: true,
  };
  const CLAUDE_WORK = {
    id: 'conn-cli',
    name: 'Claude Work',
    protocolId: 'claude-code',
    providerId: 'claude-code',
    authMode: 'subscription_login',
    baseUrl: null,
    secretRef: 'conn-cli',
    modelId: '',
    accountLabel: null,
    enabled: true,
    env: [
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN', secretRef: 'provider-connection::conn-cli::env::API_TOKEN', secret: true },
    ],
  };
  const API_WORK = {
    id: 'conn-api',
    name: 'Work OpenAI',
    protocolId: 'openai',
    providerId: 'openai',
    authMode: 'developer_api_key',
    baseUrl: null,
    secretRef: 'conn-api',
    modelId: 'gpt-5',
    accountLabel: null,
    enabled: true,
  };
  const ENV_LABEL = 'Environment variables';

  beforeEach(() => {
    client.listAvailableProviderAdapters.mockResolvedValue([CLAUDE_CODE]);
    client.listProviderConnections.mockReset().mockResolvedValue([CLAUDE_WORK, API_WORK]);
    client.createProviderConnection.mockReset().mockResolvedValue({});
    client.updateProviderConnection.mockReset().mockResolvedValue({});
  });

  const pickProvider = async (user: ReturnType<typeof userEvent.setup>, name: string) => {
    await user.click(await screen.findByRole('button', { name: '+ Add Connection' }));
    await user.click(await screen.findByText(name));
  };

  it('shows the editor for CLI connections and not for API ones', async () => {
    const user = userEvent.setup();
    openSettings();

    await pickProvider(user, 'OpenAI');
    expect(screen.getByRole('heading', { name: /Add OpenAI/ })).toBeInTheDocument();
    expect(screen.queryByText(ENV_LABEL)).toBeNull();
    await user.keyboard('{Escape}');

    await pickProvider(user, 'Claude Code');
    expect(screen.getByRole('heading', { name: /Add Claude Code/ })).toBeInTheDocument();
    expect(screen.getByText(ENV_LABEL)).toBeInTheDocument();
  });

  it('creates a CLI connection with the env rows that have a key', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Claude Code');

    await user.click(screen.getByRole('button', { name: '+ Add variable' }));
    await user.click(screen.getByRole('button', { name: '+ Add variable' }));
    await user.type(screen.getAllByPlaceholderText('KEY')[0]!, 'CLAUDE_CONFIG_DIR');
    await user.type(screen.getAllByPlaceholderText('value')[0]!, '/home/me/.claude-work');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));

    await waitFor(() => expect(client.createProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.createProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
    ]);
  });

  it('creates a CLI connection without env when no rows were added', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Claude Code');

    await user.click(screen.getByRole('button', { name: 'Add Connection' }));

    await waitFor(() => expect(client.createProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.createProviderConnection.mock.calls[0]![0].env).toBeUndefined();
  });

  it('loads stored rows and keeps a stored secret when another field changes', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    expect(screen.getAllByPlaceholderText('KEY').map((input) => (input as HTMLInputElement).value))
      .toEqual(['CLAUDE_CONFIG_DIR', 'API_TOKEN']);
    expect(screen.getByDisplayValue('/home/me/.claude-work')).toBeInTheDocument();
    expect(screen.getByPlaceholderText('stored — enter to replace')).toHaveValue('');

    const name = screen.getByLabelText('Connection Name');
    await user.clear(name);
    await user.type(name, 'Claude Work 2');
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    const request = client.updateProviderConnection.mock.calls[0]![0];
    expect(request.name).toBe('Claude Work 2');
    expect(request.env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN', secret: true },
    ]);
  });

  it('drops a removed row from the update', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.click(screen.getByRole('button', { name: 'Remove CLAUDE_CONFIG_DIR' }));
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'API_TOKEN', secret: true },
    ]);
  });

  it('sends a typed replacement for a stored secret', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.type(screen.getByPlaceholderText('stored — enter to replace'), 'new-token');
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN', value: 'new-token', secret: true },
    ]);
  });

  // Omitting env keeps the stored list, so clearing it must send [].
  it('sends an empty list when every row is removed', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.click(screen.getByRole('button', { name: 'Remove CLAUDE_CONFIG_DIR' }));
    await user.click(screen.getByRole('button', { name: 'Remove API_TOKEN' }));
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([]);
  });

  // Nothing is stored under the new key, so the row needs a new value.
  it('asks for a value after a stored secret is renamed', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    const key = screen.getByDisplayValue('API_TOKEN');
    await user.clear(key);
    await user.type(key, 'GH_TOKEN');
    expect(screen.queryByPlaceholderText('stored — enter to replace')).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Save Connection' }));
    expect(await screen.findByText('Enter a value for the secret GH_TOKEN')).toBeInTheDocument();
    expect(client.updateProviderConnection).not.toHaveBeenCalled();

    await user.type(screen.getAllByPlaceholderText('value')[1]!, 'gh-1');
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'GH_TOKEN', value: 'gh-1', secret: true },
    ]);
  });

  it('keeps a stored secret when its key is edited back', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.type(screen.getByDisplayValue('API_TOKEN'), 'X{Backspace} ');
    expect(screen.getByPlaceholderText('stored — enter to replace')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN', secret: true },
    ]);
  });

  // Saved empty, an unticked secret would become an empty plain variable.
  it('asks for a value after a stored secret is unticked', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.type(screen.getByDisplayValue('API_TOKEN'), ' ');
    await user.click(screen.getAllByRole('checkbox', { name: 'secret' })[1]!);
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));
    expect(
      await screen.findByText('Enter a value for API_TOKEN: saving it empty replaces its stored secret')
    ).toBeInTheDocument();
    expect(client.updateProviderConnection).not.toHaveBeenCalled();

    await user.type(screen.getAllByPlaceholderText('value')[1]!, 'plain');
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN', value: 'plain', secret: false },
    ]);
  });

  // Nothing is stored for a plain variable, so as a secret it needs a value.
  it('asks for a value after a stored plain variable is made secret', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.clear(screen.getByDisplayValue('/home/me/.claude-work'));
    await user.click(screen.getAllByRole('checkbox', { name: 'secret' })[0]!);
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    expect(await screen.findByText('Enter a value for the secret CLAUDE_CONFIG_DIR')).toBeInTheDocument();
    expect(client.updateProviderConnection).not.toHaveBeenCalled();
  });

  // An empty plain variable under a new name replaces nothing stored.
  it('saves a stored secret renamed and unticked with no value', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude Work'));

    await user.type(screen.getByDisplayValue('API_TOKEN'), '_2');
    await user.click(screen.getAllByRole('checkbox', { name: 'secret' })[1]!);
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toEqual([
      { key: 'CLAUDE_CONFIG_DIR', value: '/home/me/.claude-work', secret: false },
      { key: 'API_TOKEN_2', value: '', secret: false },
    ]);
  });

  it('rejects a new secret left blank', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Claude Code');

    await user.click(screen.getByRole('button', { name: '+ Add variable' }));
    await user.type(screen.getByPlaceholderText('KEY'), 'API_TOKEN');
    await user.click(screen.getByRole('checkbox', { name: 'secret' }));
    await user.type(screen.getByPlaceholderText('value'), ' ');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));

    expect(await screen.findByText('Enter a value for the secret API_TOKEN')).toBeInTheDocument();
    expect(client.createProviderConnection).not.toHaveBeenCalled();
  });

  it('leaves env out when updating an API connection', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Work OpenAI'));
    expect(screen.queryByText(ENV_LABEL)).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].env).toBeUndefined();
  });
});

describe('provider connection form', () => {
  const entry = (
    fields: Pick<ProviderCatalogEntry, 'id' | 'displayName' | 'category' | 'protocolId'> & Partial<ProviderCatalogEntry>,
  ): ProviderCatalogEntry => ({
    description: '',
    defaultBaseUrl: null,
    baseUrlLocked: false,
    requiresApiKey: true,
    logoAsset: '',
    curatedModels: [],
    docsUrl: null,
    extraHeaders: [],
    modelsEndpointStyle: 'standard',
    capabilities: null,
    ...fields,
  });
  const CATALOG = [
    entry({
      id: 'anthropic', displayName: 'Anthropic', category: 'hosted', protocolId: 'anthropic',
      defaultBaseUrl: 'https://api.anthropic.com', baseUrlLocked: true,
    }),
    entry({
      id: 'minimax', displayName: 'MiniMax', category: 'hosted', protocolId: 'anthropic',
      defaultBaseUrl: 'https://api.minimax.io/anthropic', baseUrlLocked: true,
      docsUrl: 'https://www.minimax.io/platform',
    }),
    entry({
      id: 'ollama', displayName: 'Ollama', category: 'self_hosted', protocolId: 'openai',
      defaultBaseUrl: 'http://localhost:11434/v1', requiresApiKey: false,
      docsUrl: 'https://ollama.com',
    }),
    entry({ id: 'custom-openai', displayName: 'Custom (OpenAI-compatible)', category: 'custom', protocolId: 'openai' }),
    entry({ id: 'custom-anthropic', displayName: 'Custom (Anthropic-compatible)', category: 'custom', protocolId: 'anthropic' }),
  ];
  const connection = (fields: { id: string; name: string; protocolId: string; providerId: string; baseUrl: string | null }) => ({
    authMode: 'developer_api_key',
    secretRef: fields.id,
    modelId: 'some-model',
    accountLabel: null,
    enabled: true,
    ...fields,
  });
  const CONNECTIONS = [
    connection({ id: 'c1', name: 'Claude API', protocolId: 'anthropic', providerId: 'anthropic', baseUrl: 'https://api.anthropic.com' }),
    connection({ id: 'c2', name: 'Old Claude', protocolId: 'anthropic', providerId: 'anthropic', baseUrl: null }),
    connection({ id: 'c3', name: 'Old Custom', protocolId: 'openai', providerId: 'custom-openai', baseUrl: null }),
    connection({ id: 'c4', name: 'Gateway', protocolId: 'anthropic', providerId: 'custom-anthropic', baseUrl: 'https://llm.example.com' }),
    connection({ id: 'c5', name: 'Local Ollama', protocolId: 'openai', providerId: 'ollama', baseUrl: 'http://localhost:11434/v1' }),
    connection({ id: 'c6', name: 'Old Ollama', protocolId: 'openai', providerId: 'ollama', baseUrl: null }),
    connection({ id: 'c7', name: 'MiniMax Proxy', protocolId: 'anthropic', providerId: 'minimax', baseUrl: 'https://proxy.example.com/anthropic' }),
    connection({ id: 'c8', name: 'MiniMax Slash', protocolId: 'anthropic', providerId: 'minimax', baseUrl: 'https://api.minimax.io/anthropic/' }),
    connection({ id: 'c9', name: 'MiniMax Caps', protocolId: 'anthropic', providerId: 'minimax', baseUrl: 'https://API.MiniMax.io/anthropic' }),
    connection({ id: 'c10', name: 'Old Custom Claude', protocolId: 'anthropic', providerId: 'custom-anthropic', baseUrl: null }),
  ];
  const BASE_URL_REQUIRED = 'Base URL is required for a custom provider.';

  beforeEach(() => {
    client.listAvailableProviderAdapters.mockResolvedValue([]);
    client.listProviderCatalog.mockResolvedValue(CATALOG);
    client.listProviderConnections.mockReset().mockResolvedValue(CONNECTIONS);
    client.createProviderConnection.mockReset().mockResolvedValue({});
    client.updateProviderConnection.mockReset().mockResolvedValue({});
  });

  const pickProvider = async (user: ReturnType<typeof userEvent.setup>, name: string) => {
    await user.click(await screen.findByRole('button', { name: '+ Add Connection' }));
    await user.click(await screen.findByText(name));
  };

  it('requires a base URL for a new custom connection', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Custom (OpenAI-compatible)');

    await user.type(screen.getByLabelText(/^API Key/), 'sk-test');
    await user.type(screen.getByLabelText(/^Model ID/), 'llama3');
    await user.type(screen.getByLabelText(/^Base URL/), '   ');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));
    expect(await screen.findByText(BASE_URL_REQUIRED)).toBeInTheDocument();
    expect(client.createProviderConnection).not.toHaveBeenCalled();

    await user.clear(screen.getByLabelText(/^Base URL/));
    await user.type(screen.getByLabelText(/^Base URL/), 'https://llm.example.com/v1');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));
    await waitFor(() => expect(client.createProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.createProviderConnection.mock.calls[0]![0].baseUrl).toBe('https://llm.example.com/v1');
  });

  it('enables Load models for a new custom connection only once it has a base URL', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Custom (Anthropic-compatible)');

    const loadModels = screen.getByRole('button', { name: 'Load models' });
    expect(loadModels).toBeDisabled();
    expect(loadModels).toHaveAccessibleDescription('Enter the base URL below to load models.');

    await user.type(screen.getByLabelText(/^Base URL/), 'https://llm.example.com');
    expect(loadModels).toBeEnabled();
    expect(loadModels).not.toHaveAccessibleDescription();
    expect(screen.queryByText('Enter the base URL below to load models.')).toBeNull();
  });

  it('keeps a custom connection from losing its base URL on edit', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Gateway'));

    await user.clear(screen.getByLabelText(/^Base URL/));
    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    expect(await screen.findByText(BASE_URL_REQUIRED)).toBeInTheDocument();
    expect(client.updateProviderConnection).not.toHaveBeenCalled();
    expect(screen.queryByText(/No base URL is set/)).toBeNull();
  });

  it('does not warn about the official API for a self-hosted connection without a base URL', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Old Ollama'));

    expect(screen.getByLabelText(/^Base URL/)).toHaveValue('');
    expect(screen.queryByText(/No base URL is set/)).toBeNull();
  });

  // It already reaches the protocol's official API; saving must not force a URL on it.
  it('saves a custom connection stored without a base URL unchanged', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Old Custom'));

    await user.click(screen.getByRole('button', { name: 'Save Connection' }));

    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].baseUrl).toBeNull();
  });

  it.each([
    ['Old Custom', 'https://api.openai.com/v1'],
    ['Old Custom Claude', 'https://api.anthropic.com'],
  ])('warns that %s sends its key to %s', async (name, officialUrl) => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText(name));

    const baseUrl = screen.getByLabelText(/^Base URL/);
    expect(baseUrl).toHaveAccessibleDescription(
      `No base URL is set, so requests and this connection's API key go to ${officialUrl}.`,
    );
    expect(screen.getByRole('button', { name: 'Load models' })).toBeEnabled();

    await user.type(baseUrl, 'https://llm.example.com');
    expect(screen.queryByText(/No base URL is set/)).toBeNull();
  });

  it.each([
    ['Custom (OpenAI-compatible)', 'https://llm.example.com/v1'],
    ['Custom (Anthropic-compatible)', 'https://llm.example.com'],
  ])('suggests an example base URL for %s', async (name, placeholder) => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, name);

    expect(screen.getByLabelText(/^Base URL/)).toHaveAttribute('placeholder', placeholder);
  });

  it('offers an optional API key for a keyless provider and sends it', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Ollama');

    const key = screen.getByLabelText(/^API Key/);
    expect(key).toHaveAccessibleDescription(/Only needed if the server is set up to require a key/);
    expect(screen.getByText('(optional)')).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Where do I get an API key/ })).toBeNull();
    await user.type(key, 'vllm-secret');
    await user.type(screen.getByLabelText(/^Model ID/), 'llama3');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));

    await waitFor(() => expect(client.createProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.createProviderConnection.mock.calls[0]![0].apiKey).toBe('vllm-secret');
  });

  it('creates a keyless connection without an API key', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'Ollama');

    await user.type(screen.getByLabelText(/^Model ID/), 'llama3');
    await user.click(screen.getByRole('button', { name: 'Add Connection' }));

    await waitFor(() => expect(client.createProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.createProviderConnection.mock.calls[0]![0].apiKey).toBeNull();
  });

  it('shows the API key field when editing a keyless connection', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Local Ollama'));

    expect(screen.getByLabelText(/^API Key/)).toHaveValue('');
  });

  it('links to the key docs for a provider that requires a key', async () => {
    const user = userEvent.setup();
    openSettings();
    await pickProvider(user, 'MiniMax');

    expect(screen.getByRole('link', { name: /Where do I get an API key/ })).toHaveAttribute(
      'href',
      'https://www.minimax.io/platform',
    );
  });

  it('opens the Base URL field when editing a hosted connection saved with another URL', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('MiniMax Proxy'));

    expect(screen.getByLabelText(/^Base URL/)).toHaveValue('https://proxy.example.com/anthropic');
    expect(screen.queryByRole('button', { name: 'Advanced: override' })).toBeNull();
  });

  it.each(['MiniMax Slash', 'MiniMax Caps'])(
    'treats the stored URL of %s as the default endpoint',
    async (name) => {
      const user = userEvent.setup();
      openSettings();
      await user.click(await screen.findByText(name));

      expect(screen.getByRole('button', { name: 'Advanced: override' })).toBeInTheDocument();
      expect(screen.queryByLabelText(/^Base URL/)).toBeNull();
    },
  );

  it('keeps a hosted endpoint locked when editing a connection saved at its default', async () => {
    const user = userEvent.setup();
    openSettings();
    await user.click(await screen.findByText('Claude API'));

    expect(screen.getByRole('button', { name: 'Advanced: override' })).toBeInTheDocument();
    expect(screen.queryByLabelText(/^Base URL/)).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Save Connection' }));
    await waitFor(() => expect(client.updateProviderConnection).toHaveBeenCalledTimes(1));
    expect(client.updateProviderConnection.mock.calls[0]![0].baseUrl).toBe('https://api.anthropic.com');
  });

  it("lists a connection without a stored base URL at its provider's default", async () => {
    openSettings();
    const row = (await screen.findByText('Old Claude')).closest<HTMLElement>('[role="button"]')!;

    expect(within(row).getByText('https://api.anthropic.com')).toBeInTheDocument();
  });

  it("lists a self-hosted connection without a stored base URL at the entry's default", async () => {
    openSettings();
    const row = (await screen.findByText('Old Ollama')).closest<HTMLElement>('[role="button"]')!;

    expect(within(row).getByText('http://localhost:11434/v1')).toBeInTheDocument();
  });
});
