import { useCallback, useState, type MouseEvent } from 'react';
import { faceOf } from '../Agents/agentIdentity';
import type { FleetCrew as Crew, FleetCrewMember, FleetCrewStatus } from '../../fleet/fleetModel';
import AgentAvatar from '../Agents/AgentAvatar';
import { MAIN_FACE, type Cast } from './FleetFace';
import FleetZoom, { zoomStart, type ZoomStart } from './FleetZoom';
import styles from './FleetPage.module.css';

const SAYS: Record<FleetCrewStatus, string> = {
  needs: 'needs you',
  running: 'working on',
  queued: 'up next',
  done: 'just finished',
  idle: 'idle',
};

const describe = (m: FleetCrewMember) => (m.doing ? `${m.name} ${SAYS[m.status]}: ${m.doing}` : `${m.name}, idle`);

/** What a face's sticker says; the words are in the face's label. */
const Sticker = ({ member }: { member: FleetCrewMember }) => {
  switch (member.status) {
    case 'running':
      return <span className={styles.crewSticker}>•••</span>;
    case 'needs':
      return <span className={styles.crewSticker}>!</span>;
    case 'idle':
      return <span className={styles.crewSticker}>zz</span>;
    case 'done':
      return (
        <span className={styles.crewSticker} data-level={member.level ?? undefined}>
          <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="4" aria-hidden="true">
            <path d="M5 12.5l4.5 4.5L19 7" />
          </svg>
        </span>
      );
    default:
      return null;
  }
};

interface Zoom {
  member: FleetCrewMember;
  origin: HTMLElement;
  start: ZoomStart;
}

/**
 * The strip under a card's title: every member of the workspace, Main first,
 * each with a sticker for what it is up to, so a running task never leaves
 * the card. Quiet by design: no effects. A busy face zooms out to its task.
 */
const FleetCrew = ({ crew, cast }: { crew: Crew; cast: Cast }) => {
  const [zoom, setZoom] = useState<Zoom | null>(null);
  const closed = useCallback(() => setZoom(null), []);
  const faceFor = (m: FleetCrewMember) => (m.main ? cast[MAIN_FACE] : faceOf(cast, m.seed));
  const open = (member: FleetCrewMember) => (e: MouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
    const origin = e.currentTarget;
    setZoom((z) => z ?? { member, origin, start: zoomStart(origin) });
  };

  return (
    <>
      <ul className={styles.crew} aria-label="Crew">
        {crew.shown.map((m) => {
          const face = (
            <span className={styles.crewFace} data-anchor>
              <AgentAvatar identity={faceFor(m)} size={22} ring={false} activity={m.status === 'running' ? 'running' : m.status === 'idle' ? 'idle' : 'none'} />
              <Sticker member={m} />
            </span>
          );
          return (
            <li key={m.seed} className={styles.crewMember} data-status={m.status}>
              {m.doing ? (
                <button type="button" className={styles.crewButton} title={describe(m)} aria-label={describe(m)} aria-haspopup="dialog" onClick={open(m)}>
                  {face}
                  {m.words && <span className={styles.crewWords}>{m.words}</span>}
                </button>
              ) : (
                <span className={styles.crewButton} title={describe(m)} role="img" aria-label={describe(m)}>
                  {face}
                </span>
              )}
            </li>
          );
        })}
        {crew.more.length > 0 && (
          <li className={styles.crewMore} title={crew.more.map(describe).join('\n')}>
            +{crew.more.length}
          </li>
        )}
      </ul>
      {zoom && (
        <FleetZoom
          origin={zoom.origin}
          start={zoom.start}
          faceIdentity={faceFor(zoom.member)}
          label={describe(zoom.member)}
          onClosed={closed}
        >
          <div className={styles.zoomText}>
            <p className={styles.zoomStamp}>
              <b>{zoom.member.name}</b> {SAYS[zoom.member.status]}
            </p>
            <p className={styles.crewZoomText}>{zoom.member.doing}</p>
          </div>
        </FleetZoom>
      )}
    </>
  );
};

export default FleetCrew;
