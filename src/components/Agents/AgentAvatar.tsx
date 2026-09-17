import { useMemo, type CSSProperties } from 'react';
import { useAppTheme } from '../Chat/useAppTheme';
import { identityRingColor, moodFor, renderIdentity, type AgentActivity, type AgentIdentity } from './agentIdentity';
import styles from './AgentAvatar.module.css';

export interface AgentAvatarProps {
  identity: AgentIdentity;
  /** CSS px. Default 32. */
  size?: number;
  /**
   * Drives the expression and the status ring. `none` (default) draws no
   * ring; `disabled` dims the face and draws no ring either.
   */
  activity?: AgentActivity;
  /** Accessible name; omit when the name is printed next to the face. */
  label?: string;
  className?: string;
}

const RINGED: ReadonlySet<AgentActivity> = new Set(['idle', 'running', 'attention']);

/**
 * An agent's face. A pure function of identity + theme + activity. Callers
 * typically build `identity` inline, so the memos key on its primitive
 * fields, not on the object.
 */
const AgentAvatar = ({ identity, size = 32, activity = 'none', label, className }: AgentAvatarProps) => {
  const theme = useAppTheme();
  const mood = moodFor(activity);
  const { seed, hue, generatorVersion } = identity;

  const svg = useMemo(
    () => renderIdentity({ seed, hue, generatorVersion }, { size, theme, mood }),
    [seed, hue, generatorVersion, size, theme, mood],
  );
  const ringColor = useMemo(
    () => identityRingColor({ seed, hue, generatorVersion }, theme),
    [seed, hue, generatorVersion, theme],
  );

  const classes = [
    styles.avatar,
    RINGED.has(activity) ? styles.ring : '',
    activity === 'running' ? styles.running : '',
    activity === 'attention' ? styles.attention : '',
    activity === 'disabled' ? styles.disabled : '',
    className ?? '',
  ]
    .filter(Boolean)
    .join(' ');

  const style = {
    '--agent-avatar-size': `${size}px`,
    '--agent-ring-color': ringColor,
  } as CSSProperties;

  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true };

  return (
    <span
      className={classes}
      style={style}
      data-activity={activity}
      {...a11y}
      // Safe: the generator emits only numbers and constant markup; the seed
      // is hashed, never echoed (see avatarGenerator.ts).
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
};

export default AgentAvatar;
