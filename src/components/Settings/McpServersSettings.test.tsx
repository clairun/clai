import { act, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getMcpServers: vi.fn(),
  getMcpServerCatalog: vi.fn(),
}));
vi.mock('../../api/client', () => api);
vi.mock('./McpServerFormModal', () => ({ default: () => null }));

import McpServersSettings from './McpServersSettings';

const server = (id: string, name: string) => ({
  id, name, enabled: true, transport: { type: 'stdio', command: 'echo', args: [] }, auth: { type: 'none' },
});

describe('McpServersSettings cross-window updates', () => {
  it('refreshes the already-open server list on the shared settings signal', async () => {
    api.getMcpServerCatalog.mockResolvedValue([]);
    api.getMcpServers.mockResolvedValueOnce([server('a', 'Alpha')]).mockResolvedValueOnce([server('b', 'Beta')]);
    render(<McpServersSettings />);
    expect(await screen.findByText('Alpha')).toBeInTheDocument();

    act(() => window.dispatchEvent(new CustomEvent('mcp-servers-changed')));
    expect(await screen.findByText('Beta')).toBeInTheDocument();
    expect(screen.queryByText('Alpha')).not.toBeInTheDocument();
    expect(api.getMcpServers).toHaveBeenCalledTimes(2);
  });
});
