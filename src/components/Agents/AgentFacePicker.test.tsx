import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import AgentFacePicker, {
  candidatesOf,
  chosenSeed,
  freshFaceChoice,
  shuffled,
  type FaceChoice,
} from './AgentFacePicker';
import { GENERATOR_VERSION, identityFor } from './agentIdentity';
import { hueIndexFor } from './avatarGenerator';

const CHOICE: FaceChoice = { nonce: 'nonce-a', picked: null };
const CANDIDATES = candidatesOf(CHOICE);
const CURRENT = identityFor({ id: 'def-1', avatar: { seed: 'stored', generatorVersion: GENERATOR_VERSION } });

afterEach(() => {
  vi.restoreAllMocks();
});

describe('face choice rules', () => {
  it('defaults to the first candidate when creating and to the stored face when editing', () => {
    expect(chosenSeed(CHOICE, null)).toBe(CANDIDATES[0]!);
    expect(chosenSeed(CHOICE, CURRENT)).toBeNull();
    expect(chosenSeed({ ...CHOICE, picked: CANDIDATES[3]! }, CURRENT)).toBe(CANDIDATES[3]!);
  });

  it('offers six candidates in six different hues', () => {
    expect(CANDIDATES).toHaveLength(6);
    expect(new Set(CANDIDATES.map(hueIndexFor)).size).toBe(6);
  });

  it('shuffles to a new row and only follows the user off the current face', () => {
    const created = shuffled(CHOICE, null);
    expect(created.nonce).not.toBe(CHOICE.nonce);
    expect(created.picked).toBeNull();
    expect(chosenSeed(created, null)).toBe(candidatesOf(created)[0]!);

    expect(shuffled(CHOICE, CURRENT).picked).toBeNull();

    const moved = shuffled({ ...CHOICE, picked: CANDIDATES[2]! }, CURRENT);
    expect(moved.picked).toBe(candidatesOf(moved)[0]!);
    expect(candidatesOf(moved)).not.toContain(CANDIDATES[2]);
  });

  it('starts every agent with its own nonce', () => {
    expect(freshFaceChoice()).toEqual({ nonce: expect.any(String), picked: null });
    expect(freshFaceChoice().nonce).not.toBe(freshFaceChoice().nonce);
  });
});

describe('AgentFacePicker', () => {
  it('creating: six candidates, the first one selected, no current face', () => {
    render(<AgentFacePicker choice={CHOICE} onChange={vi.fn()} />);
    const radios = screen.getAllByRole('radio');
    expect(radios).toHaveLength(6);
    expect(radios[0]).toBeChecked();
    expect(radios.slice(1).some((r) => r.getAttribute('aria-checked') === 'true')).toBe(false);
    expect(screen.queryByRole('radio', { name: 'Current face' })).toBeNull();
  });

  it('editing: the stored face leads and is selected until a candidate is picked', async () => {
    const onChange = vi.fn();
    render(<AgentFacePicker current={CURRENT} choice={CHOICE} onChange={onChange} />);
    const radios = screen.getAllByRole('radio');
    expect(radios).toHaveLength(7);
    expect(radios[0]).toHaveAccessibleName('Current face');
    expect(radios[0]).toBeChecked();

    await userEvent.click(screen.getByRole('radio', { name: 'Face 2' }));
    expect(onChange).toHaveBeenLastCalledWith({ nonce: 'nonce-a', picked: CANDIDATES[1]! });

    await userEvent.click(screen.getByRole('radio', { name: 'Current face' }));
    expect(onChange).toHaveBeenLastCalledWith({ nonce: 'nonce-a', picked: null });
  });

  it('reflects a picked candidate and shuffles through onChange', async () => {
    const onChange = vi.fn();
    render(
      <AgentFacePicker current={CURRENT} choice={{ ...CHOICE, picked: CANDIDATES[4]! }} onChange={onChange} />
    );
    expect(screen.getByRole('radio', { name: 'Face 5' })).toBeChecked();
    expect(screen.getByRole('radio', { name: 'Current face' })).not.toBeChecked();

    await userEvent.click(screen.getByRole('button', { name: 'Shuffle' }));
    const next = onChange.mock.calls[0]![0] as FaceChoice;
    expect(next.nonce).not.toBe('nonce-a');
    expect(next.picked).toBe(candidatesOf(next)[0]!);
  });

  it('freezes every tile and Shuffle while the host saves', () => {
    render(<AgentFacePicker current={CURRENT} choice={CHOICE} onChange={vi.fn()} disabled />);
    for (const radio of screen.getAllByRole('radio')) expect(radio).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Shuffle' })).toBeDisabled();
  });
});
