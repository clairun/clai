import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import type { FleetCrewMember } from '../../fleet/fleetModel';
import { MAIN_AVATAR_HUE } from '../Agents/agentIdentity';
import { castFor, MAIN_FACE } from './FleetFace';
import FleetCrew from './FleetCrew';

const member = (name: string, patch: Partial<FleetCrewMember> = {}): FleetCrewMember => ({
  seed: `def-${name}`,
  name,
  main: false,
  status: 'idle',
  doing: null,
  words: null,
  level: null,
  ...patch,
});
const cast = castFor({}, { accessory: 'scarf', hue: 200, screentone: 'dots' });

describe('castFor', () => {
  it("keeps Main's face the same in every workspace; only its accessory differs", () => {
    const other = castFor({}, { accessory: 'cap', hue: 320, screentone: 'lines' });
    expect(cast[MAIN_FACE]).toMatchObject({ hue: MAIN_AVATAR_HUE, accessory: 'scarf' });
    expect(other[MAIN_FACE]).toMatchObject({ hue: MAIN_AVATAR_HUE, accessory: 'cap' });
  });
});

describe('FleetCrew', () => {
  it('labels every face with what it is up to and counts the rest', () => {
    render(
      <FleetCrew
        cast={cast}
        crew={{
          shown: [
            member('Main', { seed: 'main', main: true, status: 'needs', doing: 'Ship it?' }),
            member('Writer', { status: 'running', doing: 'Draft the release notes', words: 'Draft the release…' }),
          ],
          more: [member('Idle one'), member('Idle two')],
        }}
      />
    );
    expect(screen.getByLabelText('Main needs you: Ship it?')).toBeTruthy();
    expect(screen.getByText('Draft the release…')).toBeTruthy();
    expect(screen.getByText('+2').getAttribute('title')).toBe('Idle one, idle\nIdle two, idle');
    // Main wears its workspace's accessory.
    expect(document.querySelector('[data-accessory="scarf"]')).toBeTruthy();
  });

  it('zooms a busy face out to its whole task; an idle face is only labelled', () => {
    render(<FleetCrew cast={cast} crew={{ shown: [member('Writer', { status: 'running', doing: 'Draft the release notes', words: 'Draft' }), member('Reader')], more: [] }} />);
    expect(screen.queryByRole('button', { name: /Reader/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /Writer/ }));
    expect(screen.getByRole('dialog').textContent).toContain('Draft the release notes');
  });
});
