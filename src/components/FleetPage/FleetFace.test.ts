import { describe, expect, it } from 'vitest';
import { MAIN_AVATAR_HUE, MAIN_AVATAR_SEED } from '../Agents/agentIdentity';
import { castFor, MAIN_FACE } from './FleetFace';

describe('castFor', () => {
  it("draws the card's own Main: its seed, always purple", () => {
    const cast = castFor({}, { id: 'ws-1-main', avatar: null });
    const other = castFor({}, { id: 'ws-2-main', avatar: null });
    expect(cast[MAIN_FACE]).toMatchObject({ seed: 'ws-1-main', hue: MAIN_AVATAR_HUE, main: true });
    expect(other[MAIN_FACE]).toMatchObject({ seed: 'ws-2-main', hue: MAIN_AVATAR_HUE, main: true });
    expect(castFor({}, { id: 'ws-1-main', avatar: { seed: 'picked', generatorVersion: 1 } })[MAIN_FACE].seed).toBe('picked');
    // An older snapshot that names no Main.
    expect(castFor({}, null)[MAIN_FACE].seed).toBe(MAIN_AVATAR_SEED);
  });
});
