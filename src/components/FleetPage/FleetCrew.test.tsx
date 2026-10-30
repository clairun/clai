import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import type { FleetCrewMember } from '../../fleet/fleetCrew';
import { MAIN_AVATAR_HUE } from '../Agents/agentIdentity';
import { agentAvatar } from '../Agents/avatarGenerator';
import { castFor } from './FleetFace';
import FleetCrew from './FleetCrew';

/** The generator's markup as the DOM serialises it. */
const asHtml = (svg: string) => {
  const div = document.createElement('div');
  div.innerHTML = svg;
  return div.innerHTML;
};

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
const cast = castFor({}, { id: 'ws-1-main', avatar: null });

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
    // Main's face is its own seed's, accessory included.
    expect(screen.getByLabelText('Main needs you: Ship it?').innerHTML).toContain(
      asHtml(agentAvatar('ws-1-main', { size: 22, theme: 'light', hue: MAIN_AVATAR_HUE, accessory: true }))
    );
  });

  it('zooms a busy face out to its whole task; an idle face is only labelled', () => {
    render(<FleetCrew cast={cast} crew={{ shown: [member('Writer', { status: 'running', doing: 'Draft the release notes', words: 'Draft' }), member('Reader')], more: [] }} />);
    expect(screen.queryByRole('button', { name: /Reader/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /Writer/ }));
    expect(screen.getByRole('dialog').textContent).toContain('Draft the release notes');
  });
});
