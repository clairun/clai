import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { McpServerResponse } from '../../generated/bindings';

vi.mock('../../api/client', () => ({
  cancelMcpOAuthLogin: vi.fn(),
  finishMcpOAuthLogin: vi.fn(),
  startMcpOAuthLogin: vi.fn(),
}));

import McpServerFormModal from './McpServerFormModal';

const server = (transport: McpServerResponse['transport']): McpServerResponse => ({
  id: 'srv-1',
  name: 'Files',
  enabled: true,
  transport,
  auth: { type: 'none' },
  createdAt: '2026-11-01T00:00:00Z',
  updatedAt: '2026-11-01T00:00:00Z',
  warning: null,
});

const renderForm = (initial: McpServerResponse) => {
  const onSubmit = vi.fn().mockResolvedValue(initial);
  render(
    <McpServerFormModal
      isOpen
      onClose={vi.fn()}
      onSubmit={onSubmit}
      onServerSaved={vi.fn()}
      server={initial}
    />
  );
  return { onSubmit };
};

describe('McpServerFormModal env and header editors', () => {
  it('saves stdio env with a kept secret and without empty-key rows', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'stdio',
      command: 'npx',
      args: [],
      env: [
        { key: 'ROOT', value: '/data', secret: false, hint: null },
        { key: 'TOKEN', value: null, secret: true, hint: '1234' },
      ],
    }));

    expect(screen.getByDisplayValue('/data')).toBeInTheDocument();
    expect(screen.getByPlaceholderText('••••1234 — enter to replace')).toHaveValue('');
    await user.click(screen.getByRole('button', { name: '+ Add variable' }));
    await user.click(screen.getAllByRole('checkbox', { name: 'secret' })[2]!);
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0]![0].transport.env).toEqual([
      { key: 'ROOT', value: '/data', secret: false },
      { key: 'TOKEN', secret: true },
    ]);
  });

  it('saves http headers without a removed one', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: [
        { key: 'X-Team', value: 'core', secret: false, hint: null },
        { key: 'X-Key', value: null, secret: true, hint: null },
      ],
    }));

    await user.click(screen.getByRole('button', { name: 'Remove X-Team' }));
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0]![0].transport.headers).toEqual([{ key: 'X-Key', secret: true }]);
  });

  it('sends a typed value for a new secret header', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: [],
    }));

    await user.click(screen.getByRole('button', { name: '+ Add header' }));
    await user.type(screen.getByPlaceholderText('KEY'), 'X-Key');
    await user.click(screen.getByRole('checkbox', { name: 'secret' }));
    await user.type(screen.getByPlaceholderText('value'), 'abc');
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0]![0].transport.headers).toEqual([
      { key: 'X-Key', value: 'abc', secret: true },
    ]);
  });

  it('asks for a value after a stored secret is renamed', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'stdio',
      command: 'npx',
      args: [],
      env: [{ key: 'TOKEN', value: null, secret: true, hint: '1234' }],
    }));

    await user.type(screen.getByDisplayValue('TOKEN'), '_2');
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    expect(await screen.findByText('Enter a value for the secret TOKEN_2')).toBeInTheDocument();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  // Two rows under one key would overwrite B's secret and drop A's.
  it('rejects a key used twice', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'stdio',
      command: 'npx',
      args: [],
      env: [
        { key: 'A', value: null, secret: true, hint: null },
        { key: 'B', value: null, secret: true, hint: null },
      ],
    }));

    // The padded key comes second, so the check must trim the key it looks up.
    await user.type(screen.getByDisplayValue('B'), ' ');
    const key = screen.getByDisplayValue('A');
    await user.clear(key);
    await user.type(key, 'B');
    await user.type(screen.getAllByPlaceholderText('value')[0]!, 'v');
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    expect(await screen.findByText('B is set more than once')).toBeInTheDocument();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('asks for a value after a stored secret is unticked', async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(server({
      type: 'stdio',
      command: 'npx',
      args: [],
      env: [{ key: 'TOKEN', value: null, secret: true, hint: '1234' }],
    }));

    await user.click(screen.getByRole('checkbox', { name: 'secret' }));
    await user.click(screen.getByRole('button', { name: 'Save Changes' }));

    expect(
      await screen.findByText('Enter a value for TOKEN: saving it empty replaces its stored secret')
    ).toBeInTheDocument();
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
