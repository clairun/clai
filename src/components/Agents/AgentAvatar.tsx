import { useMemo, type CSSProperties } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import {
  identityRingColor,
  moodFor,
  renderIdentity,
  type AgentActivity,
  type AgentIdentity,
} from './agentIdentity';
import styles from './AgentAvatar.module.css';

export interface AgentAvatarProps {
  identity: AgentIdentity;
  /** CSS px. Default 32. */
  size?: number;
  /**
   * Drives the expression and the status ring. `none` (default) draws no
   * ring; `disabled` dims the face and draws no ring either. Pass
   * `ring={false}` to keep the expression without the ring.
   */
  activity?: AgentActivity;
  /** Accessible name; omit when the name is printed next to the face. */
  label?: string;
  /**
   * Draw the status ring (default `true`). `false` keeps the expression — and
   * the dimming for `disabled` — but no ring, for surfaces that must not
   * animate: the `running` ring spins forever. See `.runningIndicator` in
   * `AssistantChat.module.css` for what that costs on this stack.
   */
  ring?: boolean;
  className?: string;
}

const RINGED: ReadonlySet<AgentActivity> = new Set(['idle', 'running', 'attention']);

/**
 * An agent's face. A pure function of identity + theme + activity. Callers
 * typically build `identity` inline, so the memos key on its primitive
 * fields, not on the object.
 */
const AgentAvatar = ({
  identity,
  size = 32,
  activity = 'none',
  label,
  ring = true,
  className,
}: AgentAvatarProps) => {
  const theme = useAppTheme();
  const mood = moodFor(activity);
  const { seed, hue, generatorVersion } = identity;

  const svg = useMemo(
    () => renderIdentity({ seed, hue, generatorVersion }, { size, theme, mood }),
    [seed, hue, generatorVersion, size, theme, mood]
  );
  const ringColor = useMemo(
    () => identityRingColor({ seed, hue, generatorVersion }, theme),
    [seed, hue, generatorVersion, theme]
  );

  const ringed = ring && RINGED.has(activity);
  const classes = [
    styles.avatar,
    ringed ? styles.ring : '',
    ringed && activity === 'running' ? styles.running : '',
    ringed && activity === 'attention' ? styles.attention : '',
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
