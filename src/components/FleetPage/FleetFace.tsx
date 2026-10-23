/** The faces a card draws, and its accent. */
import type { CSSProperties } from 'react';
import { mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import { oklchToHex, paletteForHue, type AvatarTheme } from '../Agents/avatarGenerator';
import type { FleetMainRef } from '../../fleet/fleetModel';

/** Where a card's cast keeps its Main; no agent definition id looks like it. */
export const MAIN_FACE = '@main';

export type Cast = Record<string, AgentIdentity> & { [MAIN_FACE]: AgentIdentity };

/** The picked faces, and this workspace's own Main. */
export const castFor = (faces: Record<string, AgentIdentity>, main: FleetMainRef | null): Cast => ({
  ...faces,
  [MAIN_FACE]: mainIdentity(main),
});

/**
 * The workspace's accent, shaded per theme as the avatars shade a hue: the
 * ring's shade for lines; for words, and for a sticker under words, the same
 * on the dark theme and a deeper step on the light one, over 4.5:1 on white
 * for every hue in `WORKSPACE_HUES`.
 */
export const accentStyle = (hue: number, theme: AvatarTheme): CSSProperties => {
  const line = paletteForHue(hue, theme).ring;
  const text = theme === 'dark' ? line : oklchToHex(0.5, 0.13, hue);
  return {
    '--ws-accent': line,
    '--ws-accent-text': text,
    '--ws-sticker': text,
    '--ws-sticker-ink': theme === 'dark' ? '#0a0e14' : '#ffffff',
  } as CSSProperties;
};
