/** The faces a card draws, their marks, and its accent. */
import type { CSSProperties, ReactNode } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { mainIdentity, type AgentActivity, type AgentIdentity } from '../Agents/agentIdentity';
import { oklchToHex, paletteForHue, type AvatarTheme } from '../Agents/avatarGenerator';
import type { FleetMainRef } from '../../fleet/fleetModel';
import { captionText, type FleetCaption } from '../../fleet/fleetFeed';
import fx, { FaceExpression, type Expression } from './FleetEffects';
import styles from './FleetPage.module.css';

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
    '--ws-sticker-ink': theme === 'dark' ? '#0a0e14' : '#ffffff',
  } as CSSProperties;
};

/** The caption box: when, worded against the page's clock. */
export const Caption = ({ caption, mood, now }: { caption: FleetCaption; mood: string; now: number }) => (
  <span className={styles.caption} data-mood={mood}>
    {captionText(caption, now)}
  </span>
);

/** A scalloped ellipse: eight arcs around the cloud's centre. */
const CLOUD = (() => {
  const points = Array.from({ length: 8 }, (_, i) => {
    const a = (i / 8) * Math.PI * 2 + 0.3;
    return [34 + Math.cos(a) * 29, 18 + Math.sin(a) * 13] as const;
  });
  let d = `M${points[0]![0].toFixed(1)} ${points[0]![1].toFixed(1)}`;
  for (let i = 1; i <= 8; i++) {
    const [x, y] = points[i % 8]!;
    const [px, py] = points[i - 1]!;
    const r = (Math.hypot(x - px, y - py) * 0.62).toFixed(1);
    d += ` A${r} ${r} 0 0 1 ${x.toFixed(1)} ${y.toFixed(1)}`;
  }
  return `${d}Z`;
})();

export const ThoughtCloud = ({ name }: { name: string }) => (
  <svg className={styles.think} width="54" height="26" viewBox="0 0 96 40" role="img" aria-label={`${name} is working`}>
    <path d={CLOUD} className={styles.thinkCloud} />
    <circle cx="24" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="34" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="44" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="74" cy="27" r="4.2" className={styles.thinkCloud} />
    <circle cx="86" cy="33" r="2.6" className={styles.thinkCloud} />
  </svg>
);

export const Who = ({
  identity,
  name,
  size,
  activity = 'none',
  label,
  expression,
  ghosts = false,
  children,
}: {
  identity: AgentIdentity;
  name: string;
  size: number;
  activity?: AgentActivity;
  /** What the face is doing, for a tooltip. */
  label?: string;
  /** A reaction drawn over the face. */
  expression?: Expression;
  /** Afterimages: the face rushing in. */
  ghosts?: boolean;
  children?: ReactNode;
}) => (
  <figure className={styles.who} title={label}>
    <span className={`${fx.actor} ${styles.face}`} style={{ width: size, height: size }} data-anchor>
      {ghosts &&
        [0, 1].map((i) => (
          <span key={i} className={fx.ghost} aria-hidden="true">
            <AgentAvatar identity={identity} size={size} activity={activity} ring={false} />
          </span>
        ))}
      <AgentAvatar identity={identity} size={size} activity={activity} ring={false} />
      {expression && <FaceExpression kind={expression} size={size} />}
    </span>
    {children}
    <figcaption>{name}</figcaption>
  </figure>
);
