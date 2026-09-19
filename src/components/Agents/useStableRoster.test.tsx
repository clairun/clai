import { describe, expect, it } from 'vitest';
import { renderHook } from '@testing-library/react';

import { useStableRoster } from './useStableRoster';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

const agent = (over: Partial<WorkspaceAgentResponse> = {}): WorkspaceAgentResponse => ({
  id: 'wa-review',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def-review',
  displayName: 'Rust Reviewer',
  role: 'member',
  enabled: true,
  isDefault: false,
  agentName: null,
  agentDescription: null,
  providerConnectionIds: [],
  skillIds: [],
  selectedMcpServerIds: [],
  execution: {},
  avatar: null,
  createdAt: 1n,
  updatedAt: 1n,
  ...over,
});

describe('useStableRoster', () => {
  it('keeps one reference when a poll rebuilt the same crew', () => {
    // The snapshot is rebuilt from scratch every 5s. An unchanged crew arriving
    // as a new array must not reach the chat as a new reference, or every face
    // and task card below it repaints four times a minute.
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] }) => useStableRoster(agents),
      { initialProps: { agents: [agent()] } }
    );
    const first = result.current;
    rerender({ agents: [agent()] });
    expect(result.current).toBe(first);
  });

  it('ignores fields no face is drawn from', () => {
    // `updatedAt` moves whenever anything about the agent row changes; the
    // faces do not read it, so it must not churn the reference.
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] }) => useStableRoster(agents),
      { initialProps: { agents: [agent()] } }
    );
    const first = result.current;
    rerender({ agents: [agent({ updatedAt: 99n, enabled: false })] });
    expect(result.current).toBe(first);
  });

  it('hands over a new crew when a face changes', () => {
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] }) => useStableRoster(agents),
      { initialProps: { agents: [agent()] } }
    );
    const first = result.current;
    rerender({ agents: [agent({ avatar: { seed: 'new-seed', generatorVersion: 1 } })] });
    expect(result.current).not.toBe(first);
    expect(result.current[0]?.avatar?.seed).toBe('new-seed');
  });

  it('renames, joins and departures each produce a new crew', () => {
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] }) => useStableRoster(agents),
      { initialProps: { agents: [agent()] } }
    );
    const first = result.current;
    rerender({ agents: [agent({ displayName: 'Reviewer' })] });
    expect(result.current).not.toBe(first);

    const renamed = result.current;
    rerender({ agents: [agent({ displayName: 'Reviewer' }), agent({ id: 'wa-2' })] });
    expect(result.current).not.toBe(renamed);
    expect(result.current).toHaveLength(2);
  });

  it('notices the two fields a face falls back on: the id and Main-ness', () => {
    // `agentIdentity` seeds a generated face from the agent id and labels the
    // workspace main differently, so neither may be dropped from the key.
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] }) => useStableRoster(agents),
      { initialProps: { agents: [agent()] } }
    );
    const first = result.current;
    rerender({ agents: [agent({ id: 'wa-other' })] });
    expect(result.current).not.toBe(first);

    const byId = result.current;
    rerender({ agents: [agent({ id: 'wa-other', isDefault: true })] });
    expect(result.current).not.toBe(byId);
  });

  it('holds one empty crew while the snapshot is still loading', () => {
    const { result, rerender } = renderHook(
      ({ agents }: { agents: WorkspaceAgentResponse[] | undefined }) => useStableRoster(agents),
      { initialProps: { agents: undefined as WorkspaceAgentResponse[] | undefined } }
    );
    const first = result.current;
    expect(first).toHaveLength(0);
    rerender({ agents: undefined });
    expect(result.current).toBe(first);
  });
});
