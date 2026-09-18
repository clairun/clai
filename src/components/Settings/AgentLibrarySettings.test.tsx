import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useImperativeHandle } from 'react';

const api = vi.hoisted(() => ({
  getMcpServers: vi.fn(),
  getSkills: vi.fn(),
  listAgentDefinitions: vi.fn(),
  saveAgentDefinition: vi.fn(),
  workspaceAgentDefaultExecution: vi.fn(),
}));
vi.mock('../../api/client', () => api);
vi.mock('../../assistant', () => ({
  assistantClient: { listProviderConnections: vi.fn().mockResolvedValue([]) },
}));

// The behaviour form has its own tests. Here it is a stub that submits a
// fixed payload and reports a rejected save the way the real form does, so
// what is under test is what the library adds around it.
const PAYLOAD = {
  workspaceId: '',
  name: 'Reviewer',
  description: 'Reviews diffs',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: ['conn-a'],
  execution: {},
  enabled: true,
};
vi.mock('./AgentBehaviorForm', () => ({
  AgentBehaviorForm: ({
    ref,
    saveBehavior,
  }: {
    ref: React.Ref<{ validate: () => unknown; submit: () => Promise<unknown> }>;
    saveBehavior: (payload: typeof PAYLOAD) => Promise<void>;
  }) => {
    useImperativeHandle(ref, () => ({
      validate: () => ({ ok: true }),
      submit: async () => {
        try {
          await saveBehavior(PAYLOAD);
          return { ok: true };
        } catch (err) {
          return { ok: false, error: String(err) };
        }
      },
    }));
    return <div data-testid="behavior-form" />;
  },
}));

import AgentLibrarySettings from './AgentLibrarySettings';
import AgentAvatar from '../Agents/AgentAvatar';
import { candidatesOf } from '../Agents/AgentFacePicker';
import { GENERATOR_VERSION, avatarRefFor, identityFor } from '../Agents/agentIdentity';

const NONCE = '11111111-1111-4111-8111-111111111111';
const SECOND_NONCE = '22222222-2222-4222-8222-222222222222';
const REVIEWER = {
  id: 'def-1',
  revision: 3,
  archived: false,
  name: 'Reviewer',
  description: 'Reviews diffs',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: ['conn-a'],
  execution: {},
  enabled: true,
  avatar: { seed: 'stored-face', generatorVersion: GENERATOR_VERSION },
  createdAt: 1,
  updatedAt: 1,
  assignedWorkspaces: [],
};

const lastSave = () => api.saveAgentDefinition.mock.calls.at(-1)?.[0];

/** The SVG `AgentAvatar` draws for an identity at the tile size, for comparing faces. */
const faceSvg = (identity: ReturnType<typeof identityFor>) =>
  render(<AgentAvatar identity={identity} size={40} />).container.querySelector('svg')!.outerHTML;

describe('AgentLibrarySettings faces', () => {
  beforeEach(() => {
    vi.spyOn(crypto, 'randomUUID').mockReturnValue(NONCE);
    api.getMcpServers.mockResolvedValue([]);
    api.getSkills.mockResolvedValue([]);
    api.workspaceAgentDefaultExecution.mockResolvedValue(null);
    api.listAgentDefinitions.mockResolvedValue([REVIEWER]);
    api.saveAgentDefinition.mockResolvedValue('def-1');
  });

  it('a new agent is created with the first candidate face unless another is picked', async () => {
    render(<AgentLibrarySettings />);
    await screen.findByRole('button', { name: /^Reviewer/ });
    const candidates = candidatesOf({ nonce: NONCE, picked: null });

    expect(screen.getByRole('radio', { name: 'Face 1' })).toBeChecked();
    await userEvent.click(screen.getByRole('button', { name: 'Create agent' }));
    await waitFor(() => expect(api.saveAgentDefinition).toHaveBeenCalledTimes(1));
    expect(lastSave()).toMatchObject({ name: 'Reviewer', avatar: avatarRefFor(candidates[0]!) });
    expect(lastSave().id).toBeUndefined();

    // The created agent is open now, so "+ New agent" is a real switch again.
    await waitFor(() => expect(screen.getByRole('radio', { name: 'Current face' })).toBeChecked());
    await userEvent.click(screen.getByRole('button', { name: '+ New agent' }));
    expect(screen.queryByRole('radio', { name: 'Current face' })).toBeNull();
    expect(screen.getByRole('radio', { name: 'Face 1' })).toBeChecked();
  });

  it('Shuffle replaces the row and the save follows the face now on screen', async () => {
    render(<AgentLibrarySettings />);
    await screen.findByRole('button', { name: /^Reviewer/ });
    const before = faceSvg({ seed: candidatesOf({ nonce: NONCE, picked: null })[0]!, generatorVersion: GENERATOR_VERSION });
    const shuffled = candidatesOf({ nonce: SECOND_NONCE, picked: null });
    expect(screen.getByRole('radio', { name: 'Face 1' }).innerHTML).toContain(before);

    vi.mocked(crypto.randomUUID).mockReturnValueOnce(SECOND_NONCE);
    await userEvent.click(screen.getByRole('button', { name: 'Shuffle' }));
    const first = screen.getByRole('radio', { name: 'Face 1' });
    expect(first).toBeChecked();
    expect(first.innerHTML).not.toContain(before);
    expect(first.innerHTML).toContain(faceSvg({ seed: shuffled[0]!, generatorVersion: GENERATOR_VERSION }));

    await userEvent.click(screen.getByRole('button', { name: 'Create agent' }));
    await waitFor(() => expect(api.saveAgentDefinition).toHaveBeenCalledTimes(1));
    expect(lastSave()).toMatchObject({ avatar: avatarRefFor(shuffled[0]!) });
  });

  it('saving an existing agent without touching the row keeps its stored face', async () => {
    render(<AgentLibrarySettings />);
    await userEvent.click(await screen.findByRole('button', { name: /^Reviewer/ }));
    const currentTile = screen.getByRole('radio', { name: 'Current face' });
    expect(currentTile).toBeChecked();
    // The tile shows the face the agent has, not the one its id would hash to.
    expect(currentTile.innerHTML).toContain(faceSvg(identityFor({ id: 'def-1', avatar: REVIEWER.avatar })));
    expect(currentTile.innerHTML).not.toContain(faceSvg(identityFor({ id: 'def-1' })));

    // Browsing without picking, then saving something else, leaves the row alone.
    vi.mocked(crypto.randomUUID).mockReturnValueOnce(SECOND_NONCE);
    await userEvent.click(screen.getByRole('button', { name: 'Shuffle' }));
    const browsed = screen.getByRole('radio', { name: 'Face 1' }).innerHTML;
    await userEvent.click(screen.getByRole('button', { name: 'Save agent' }));
    await waitFor(() => expect(api.saveAgentDefinition).toHaveBeenCalledTimes(1));
    expect(lastSave()).toMatchObject({ id: 'def-1', expectedRevision: 3 });
    expect(lastSave()).not.toHaveProperty('avatar');
    expect(screen.getByRole('radio', { name: 'Current face' })).toBeChecked();
    expect(screen.getByRole('radio', { name: 'Face 1' }).innerHTML).toBe(browsed);
  });

  it('a picked candidate replaces the stored face, and the row starts over per agent', async () => {
    render(<AgentLibrarySettings />);
    await userEvent.click(await screen.findByRole('button', { name: /^Reviewer/ }));
    const candidates = candidatesOf({ nonce: NONCE, picked: null });

    await userEvent.click(screen.getByRole('radio', { name: 'Face 3' }));
    // Clicking the agent already open is not a switch: the pick stays.
    await userEvent.click(screen.getByRole('button', { name: /^Reviewer/ }));
    expect(screen.getByRole('radio', { name: 'Face 3' })).toBeChecked();
    await userEvent.click(screen.getByRole('button', { name: 'Save agent' }));
    await waitFor(() => expect(api.saveAgentDefinition).toHaveBeenCalledTimes(1));
    expect(lastSave()).toMatchObject({ id: 'def-1', avatar: avatarRefFor(candidates[2]!) });
    // Saved: the row is back on the (now updated) stored face.
    await waitFor(() => expect(screen.getByRole('radio', { name: 'Current face' })).toBeChecked());

    // Clicking the agent already open is not a switch: the pick stays.
    await userEvent.click(screen.getByRole('radio', { name: 'Face 4' }));
    await userEvent.click(screen.getByRole('button', { name: /^Reviewer/ }));
    expect(screen.getByRole('radio', { name: 'Face 4' })).toBeChecked();

    // Picking for one agent must not leak into the next one opened.
    await userEvent.click(screen.getByRole('button', { name: '+ New agent' }));
    expect(screen.queryByRole('radio', { name: 'Current face' })).toBeNull();
    expect(screen.getByRole('radio', { name: 'Face 1' })).toBeChecked();
  });

  it('a rejected save freezes the row while in flight and then keeps the pick', async () => {
    let reject: (reason: unknown) => void = () => {};
    api.saveAgentDefinition.mockImplementationOnce(
      () => new Promise((_resolve, rejectSave) => { reject = rejectSave; })
    );
    render(<AgentLibrarySettings />);
    await userEvent.click(await screen.findByRole('button', { name: /^Reviewer/ }));
    await userEvent.click(screen.getByRole('radio', { name: 'Face 3' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save agent' }));

    await waitFor(() => expect(screen.getByRole('radio', { name: 'Face 3' })).toBeDisabled());
    expect(screen.getByRole('button', { name: 'Shuffle' })).toBeDisabled();

    reject('This agent changed somewhere else.');
    expect(await screen.findByRole('alert')).toHaveTextContent('This agent changed somewhere else.');
    expect(screen.getByRole('radio', { name: 'Face 3' })).toBeChecked();
    expect(screen.getByRole('radio', { name: 'Face 3' })).toBeEnabled();
  });
});
