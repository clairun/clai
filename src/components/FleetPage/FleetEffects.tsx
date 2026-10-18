/**
 * The Fleet's drawn effects: decoration only, `aria-hidden`, every state is
 * also in words. They are styled in `FleetEffects.module.css` and shown only
 * under `data-fx="on"` (`FLEET_FX`), so turning them down is one change.
 */
import type { ReactElement } from 'react';
import type { FleetShout } from '../../fleet/fleetModel';
import fx from './FleetEffects.module.css';

/** `'off'` keeps the comic page and drops every drawn effect. */
export const FLEET_FX: 'on' | 'off' = 'on';

export type Expression = 'working' | 'asking' | 'done' | 'asleep';

const INK = 'var(--fx-ink)';
const SWEAT = 'var(--color-info-light)';
const SPARKLE = 'var(--color-warning-light)';

/** Drawn over a face in its own 100 × 100 box; marks may leave the box. */
const EXPRESSIONS: Record<Expression, ReactElement> = {
  working: (
    <>
      <path d="M26 35l15 5M74 35l-15 5" stroke={INK} strokeWidth="5" strokeLinecap="round" />
      <path d="M90 14l12-10M96 26l14-4M84 6l4-14" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
    </>
  ),
  asking: <path d="M92 20c6 9 9 14 9 18a9 9 0 0 1-18 0c0-4 3-9 9-18z" fill={SWEAT} stroke={INK} strokeWidth="3" />,
  done: (
    <>
      <path d="M92 0l3 9 9 3-9 3-3 9-3-9-9-3 9-3z" fill={SPARKLE} stroke={INK} strokeWidth="2" />
      <path d="M4 22l2 6 6 2-6 2-2 6-2-6-6-2 6-2z" fill={SPARKLE} stroke={INK} strokeWidth="1.5" />
      <path d="M100 52l1.6 5 5 1.6-5 1.6-1.6 5-1.6-5-5-1.6 5-1.6z" fill={SPARKLE} stroke={INK} strokeWidth="1.2" />
    </>
  ),
  asleep: (
    <>
      <text x="76" y="14" fontSize="18" fontWeight="900" fill="currentColor" opacity=".6">
        z
      </text>
      <text x="88" y="0" fontSize="24" fontWeight="900" fill="currentColor" opacity=".45">
        Z
      </text>
    </>
  ),
};

export const FaceExpression = ({ kind, size }: { kind: Expression; size: number }) => (
  <svg className={fx.expression} viewBox="0 0 100 100" width={size} height={size} aria-hidden="true">
    {EXPRESSIONS[kind]}
  </svg>
);

/** An outlined sound effect: "GO!", "DONE!", "!?". */
export const Sfx = ({ word, tone, level = 'full', className = '' }: { word: string; tone: 'go' | 'done' | 'warn'; level?: FleetShout; className?: string }) =>
  level ? (
    <span className={`${fx.sfx} ${className}`} data-tone={tone} data-level={level} aria-hidden="true">
      {word}
    </span>
  ) : null;

/** Speed lines trailing the agent that rushes in. */
export const SpeedLines = () => <span className={fx.speed} aria-hidden="true" />;

export default fx;
