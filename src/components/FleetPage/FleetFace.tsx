/**
 * A face on the Fleet: the agent's own, and for a workspace's Main the
 * accessory that workspace gives it (`workspaceLook`). Not an effect: it
 * stays when the drawn effects are off.
 */
import type { CSSProperties, ReactElement } from 'react';
import AgentAvatar, { type AgentAvatarProps } from '../Agents/AgentAvatar';
import { mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import { oklchToHex, paletteForHue, type AvatarTheme } from '../Agents/avatarGenerator';
import type { Accessory, WorkspaceLook } from '../../fleet/workspaceLook';
import styles from './FleetPage.module.css';

export type CastIdentity = AgentIdentity & { accessory?: Accessory };
export type Cast = Record<string, CastIdentity>;

/** Where a card's cast keeps its Main; no agent definition id looks like it. */
export const MAIN_FACE = '@main';

/** The picked faces, and this workspace's Main with its accessory; Main's face is the same everywhere. */
export const castFor = (faces: Record<string, AgentIdentity>, look: WorkspaceLook): Cast => ({
  ...faces,
  [MAIN_FACE]: { ...mainIdentity(), accessory: look.accessory },
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

const INK = 'var(--fx-ink, #0a0e14)';
const PAPER = '#f5f1e8';

/** Drawn over Main's face in its 100 × 100 box: the body spans 18–82 across, 22–86 down; the visor sits at 44–50. */
const ACCESSORY: Record<Accessory, ReactElement> = {
  headband: (
    <>
      <rect x="17" y="29" width="66" height="8" rx="3" fill={INK} />
      <path d="M82 33 L96 26 M82 34 L95 42" stroke={INK} strokeWidth="4" strokeLinecap="round" />
    </>
  ),
  glasses: (
    <g fill="rgba(245,241,232,.35)" stroke={INK} strokeWidth="3.5">
      <circle cx="38" cy="47" r="10" />
      <circle cx="62" cy="47" r="10" />
      <path d="M48 46 h4" fill="none" />
    </g>
  ),
  cap: (
    <>
      <path d="M17 38 C17 14 83 14 83 38 Z" fill={INK} />
      <path d="M60 34 h34 a4 4 0 0 1 0 8 h-34 z" fill={INK} />
    </>
  ),
  bow: (
    <g fill={PAPER} stroke={INK} strokeWidth="2.5" strokeLinejoin="round">
      <path d="M72 22 L58 13 L60 31 Z M72 22 L86 13 L84 31 Z" />
      <circle cx="72" cy="22" r="4" fill={INK} />
    </g>
  ),
  scarf: (
    <>
      <path d="M16 78 Q50 92 84 78 L84 90 Q50 104 16 90 Z" fill={INK} />
      <path d="M62 88 l4 14 h8 l-3 -16 z" fill={INK} />
    </>
  ),
  earphones: (
    <g fill={INK}>
      <path d="M17 50 C17 4 83 4 83 50" fill="none" stroke={INK} strokeWidth="4" />
      <rect x="9" y="44" width="11" height="18" rx="4" />
      <rect x="80" y="44" width="11" height="18" rx="4" />
    </g>
  ),
  scar: <path d="M60 56 L72 68 M72 56 L60 68" stroke={INK} strokeWidth="3" strokeLinecap="round" />,
  flower: (
    <g stroke={INK} strokeWidth="2">
      {[0, 72, 144, 216, 288].map((a) => (
        <circle key={a} cx={24 + 7 * Math.cos((a * Math.PI) / 180)} cy={26 + 7 * Math.sin((a * Math.PI) / 180)} r="5.5" fill={PAPER} />
      ))}
      <circle cx="24" cy="26" r="4" fill="var(--color-warning)" />
    </g>
  ),
};

/** `AgentAvatar`, with the accessory a workspace's Main wears. */
const FleetFace = ({ identity, size = 32, ...rest }: Omit<AgentAvatarProps, 'identity'> & { identity: CastIdentity }) => {
  const accessory = identity.accessory;
  if (!accessory) return <AgentAvatar identity={identity} size={size} {...rest} />;
  return (
    <span className={styles.castFace} style={{ width: size, height: size }} data-accessory={accessory}>
      <AgentAvatar identity={identity} size={size} {...rest} />
      <svg className={styles.accessory} viewBox="0 0 100 100" width={size} height={size} aria-hidden="true">
        {ACCESSORY[accessory]}
      </svg>
    </span>
  );
};

export default FleetFace;
