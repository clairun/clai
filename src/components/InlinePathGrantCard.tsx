import { useState, type ReactNode, type Ref } from 'react';
import { containerEngineWarning } from '../permissions/containerEngineSocket';
import { listPendingPathGrantRequests, submitPathGrantDecision } from '../permissions/pathGrantsClient';
import type {
  FilesystemPathAccess,
  PathGrantDecision,
  PathGrantRequest,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { requestingAgent, type SenderAgent } from './AssistantChat/messageSender';
import RequestingAgent from './Agents/RequestingAgent';
import { chipText } from './requestChips';
import { useDecisionSender } from './useDecisionSender';
import { ResolvedRequestNotice } from './ResolvedRequestNotice';
import { PendingRequestStack, type PendingRequestSource } from './PendingRequestStack';
import styles from './InlinePathGrantCard.module.css';

const PATH_GRANT_REQUEST_EVENT = 'path-grants://request';
const PATH_GRANT_RESOLVED_EVENT = 'path-grants://resolved';

type PathAccess = FilesystemPathAccess;

const denyDecision = (): PathGrantDecision => ({ kind: 'deny' });
const allowOnceDecision = (path: string, access: PathAccess): PathGrantDecision => ({
  kind: 'allowOnce',
  path,
  access,
});
const allowAlwaysDecision = (path: string, access: PathAccess): PathGrantDecision => ({
  kind: 'allowAlways',
  path,
  access,
  scope: 'agent',
});

interface PathGrantCardState {
  path: string;
  access: PathAccess;
}

// Paths that historically hold credentials or secrets. Surfacing this in
// the modal helps the user decide whether to approve a grant they didn't
// initiate themselves. We match these as either an exact path or a path
// starting with `<known>/`, anchored at the end of the user's HOME (so
// `/home/foo/.ssh` and `/Users/bar/.ssh` both trip the warning) AND
// also as suffix matches anywhere (so `/mnt/keys/.ssh` is still flagged
// if someone keeps SSH keys elsewhere).
const SENSITIVE_SUFFIXES = [
  '/.ssh',
  '/.gnupg',
  '/.password-store',
  '/.aws',
  '/.config/gcloud',
  '/.kube',
  '/.docker',
  '/.netrc',
  '/.npmrc',
  '/.pypirc',
];

// Paths that are likely too broad: they expose the user's entire identity
// or system. Anything that's an ancestor of (or equal to) these gets a
// distinct red warning, separate from "credentials" yellow.
const OVERLY_BROAD_PATHS = ['/', '/home', '/Users', '/etc', '/usr', '/var'];

const isSensitivePath = (path: string | null | undefined): boolean => {
  if (!path) return false;
  return SENSITIVE_SUFFIXES.some(
    (suffix) => path === suffix || path.endsWith(suffix) || path.includes(`${suffix}/`),
  );
};

const isOverlyBroadPath = (path: string | null | undefined): boolean => {
  if (!path) return false;
  const normalized = path.replace(/\/+$/, '') || '/';
  if (OVERLY_BROAD_PATHS.includes(normalized)) return true;
  const homeRootMatch = /^\/(home|Users)\/[^/]+$/.test(normalized);
  return homeRootMatch;
};


const accessLabel = (access: PathAccess): string =>
  access === 'read_write' ? 'Read + write' : 'Read only';

interface PathGrantCardProps {
  req: PathGrantRequest;
  /**
   * Draw the request as the agent saying it (the Fleet's asking face and
   * bubble): no tag or collapse, and short decision chips (full labels in
   * the tooltips) below. The warnings stay; narrowing the path is left to
   * the full card.
   */
  speaker?: (prompt: ReactNode) => ReactNode;
  /** The agent that asked, drawn in the header; the spoken card draws its own face. */
  requester?: SenderAgent | null;
  /** The decision was accepted. */
  onDecided?: (requestId: string) => void;
  ref?: Ref<HTMLElement>;
}

/** One path-grant request and its decision. */
export const PathGrantCard = ({ req, speaker, requester, onDecided, ref }: PathGrantCardProps) => {
  const [card, setCard] = useState<PathGrantCardState>({ path: req.requestedPath, access: req.requestedAccess });
  const { send: sendDecision, isSubmitting, error, resolved } = useDecisionSender(
    (decision: PathGrantDecision) => submitPathGrantDecision(req.requestId, decision),
    () => onDecided?.(req.requestId),
  );
  // Collapse: squish the card to a one-line summary so a tall (or stacked)
  // request can't swallow the conversation. Default expanded.
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);


  // Validation: edited path must be the original or a descendant
  // (component-wise prefix). Edited access must be no stronger
  // than requested.
  const requestedPath = req.requestedPath;
  const isNarrower =
    card.path === requestedPath ||
    card.path.startsWith(`${requestedPath}${requestedPath.endsWith('/') ? '' : '/'}`);
  const accessIsAllowed =
    !(req.requestedAccess === 'read_only' && card.access === 'read_write');

  const pathInvalid = !card.path.trim() || !isNarrower;
  const accessInvalid = !accessIsAllowed;
  const canSubmit = !pathInvalid && !accessInvalid && !isSubmitting;

  const sensitive = isSensitivePath(card.path);
  const overlyBroad = isOverlyBroadPath(card.path);
  const containerEngine = containerEngineWarning(card.path);

  return (
    <article
      ref={ref}
      tabIndex={-1}
      className={spoken ? `${styles.card} ${styles.spoken}` : styles.card}
      aria-label={requester ? `Path grant requested by ${requester.name}` : 'Path grant requested'}
    >
      {!spoken && (
        <header className={styles.cardHeader}>
          <div className={styles.cardHeaderLeft}>
            <span className={styles.tag}>Path access requested</span>
            <span className={styles.cardSubtitle}>
              {requester ? (
                <>
                  <RequestingAgent agent={requester} /> wants to extend its filesystem grants:
                </>
              ) : (
                'An agent wants to extend its filesystem grants:'
              )}
            </span>
          </div>
          <button
            type="button"
            className={styles.collapseBtn}
            onClick={() => setCollapsed((c) => !c)}
            aria-expanded={!isCollapsed}
            aria-label={isCollapsed ? 'Expand request' : 'Collapse request'}
            title={isCollapsed ? 'Expand' : 'Collapse'}
          >
            {isCollapsed ? '▸' : '▾'}
          </button>
        </header>
      )}

      {isCollapsed ? (
        <div className={styles.collapsedSummary}>{card.path}</div>
      ) : (
        <div className={styles.cardBody}>
          {speaker ? (
            speaker(
              <>
                May I {req.requestedAccess === 'read_write' ? 'read and write' : 'read'}{' '}
                <code className={styles.spokenPath}>{req.requestedPath}</code>?
                <span className={styles.spokenReason}>{req.reason || '(no reason given)'}</span>
              </>
            )
          ) : (
            <>
              <div className={styles.reasonBlock}>
                <span className={styles.reasonLabel}>Reason from agent:</span>
                <div className={styles.reasonText}>{req.reason || '(no reason given)'}</div>
              </div>

              <section className={styles.fields}>
                <label className={styles.fieldLabel}>
                  Path (you can narrow to a more specific subpath, but not widen)
                  <input
                    type="text"
                    className={`${styles.pathInput} ${pathInvalid ? styles.inputInvalid : ''}`}
                    value={card.path}
                    onChange={(e) => setCard({ ...card, path: e.target.value })}
                    disabled={isSubmitting}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </label>
                {pathInvalid && card.path.trim() && (
                  <div className={styles.validationError}>
                    Path must be the requested path or a descendant of it. Requested:{' '}
                    <code>{requestedPath}</code>
                  </div>
                )}

                <div className={styles.accessRow}>
                  <span className={styles.fieldLabelInline}>Access:</span>
                  <label className={styles.radio}>
                    <input
                      type="radio"
                      name={`access-${req.requestId}`}
                      value="read_only"
                      checked={card.access === 'read_only'}
                      onChange={() => setCard({ ...card, access: 'read_only' })}
                      disabled={isSubmitting}
                    />
                    Read only
                  </label>
                  <label
                    className={`${styles.radio} ${
                      req.requestedAccess === 'read_only' ? styles.radioDisabled : ''
                    }`}
                    title={
                      req.requestedAccess === 'read_only'
                        ? "The agent asked for read-only; you can't upgrade it here."
                        : undefined
                    }
                  >
                    <input
                      type="radio"
                      name={`access-${req.requestId}`}
                      value="read_write"
                      checked={card.access === 'read_write'}
                      onChange={() => setCard({ ...card, access: 'read_write' })}
                      disabled={isSubmitting || req.requestedAccess === 'read_only'}
                    />
                    Read + write
                  </label>
                  <span className={styles.requestedHint}>
                    Agent requested:{' '}
                    <strong>{accessLabel(req.requestedAccess)}</strong>
                  </span>
                </div>
              </section>
            </>
          )}

          {(sensitive || overlyBroad || containerEngine) && (
            <div className={styles.warningStack}>
              {overlyBroad && (
                <div className={`${styles.warning} ${styles.warningCritical}`}>
                  ⚠ This is a very broad path. It exposes the user's entire account or
                  system to the agent. Consider narrowing to a specific subdirectory.
                </div>
              )}
              {containerEngine && (
                <div className={`${styles.warning} ${styles.warningCritical}`}>⚠ {containerEngine}</div>
              )}
              {/* ~/.docker holds credentials, but the socket under it is the bigger risk. */}
              {sensitive && !overlyBroad && !containerEngine && (
                <div className={`${styles.warning} ${styles.warningCaution}`}>
                  ⚠ This path typically holds credentials or secrets (e.g. SSH keys,
                  cloud tokens). With network enabled, anything readable here can be
                  exfiltrated by tools the agent runs. Grant carefully.
                </div>
              )}
            </div>
          )}

          {resolved ? (
            <ResolvedRequestNotice onDismiss={onDecided && (() => onDecided(req.requestId))} />
          ) : (
            <div className={styles.buttonRow}>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowOnce}`}
                onClick={() => sendDecision(allowOnceDecision(card.path.trim(), card.access))}
                disabled={!canSubmit}
                {...chipText(spoken, 'Allow once', 'Once', 'Grant for the rest of this run only. Vanishes when the run ends.')}
              />
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowAlways}`}
                onClick={() => sendDecision(allowAlwaysDecision(card.path.trim(), card.access))}
                disabled={!canSubmit}
                {...chipText(
                  spoken,
                  'Always allow (this agent)',
                  'Always',
                  "Add to this agent's permanent grant list. Survives across runs."
                )}
              />
              <button
                type="button"
                className={`${styles.btn} ${styles.btnDeny}`}
                onClick={() => sendDecision(denyDecision())}
                disabled={isSubmitting}
                {...chipText(spoken, 'Deny', 'Deny')}
              />
            </div>
          )}

          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
        </div>
      )}
    </article>
  );
};

const PATH_GRANT_REQUESTS: PendingRequestSource<PathGrantRequest> = {
  list: listPendingPathGrantRequests,
  requestEvent: PATH_GRANT_REQUEST_EVENT,
  resolvedEvent: PATH_GRANT_RESOLVED_EVENT,
  isRequest: (req) => Boolean(req.requestedPath && req.requestedAccess),
};

/**
 * The workspace's pending path grants, one card each with an editable path
 * and access. The card enforces the narrowing rules (no widening, no
 * read-only to read-write) so the user never meets a backend rejection.
 */
const InlinePathGrantCard = ({
  workspaceId,
  roster = [],
  loadedTasks,
}: {
  workspaceId: string | null;
  /** The workspace's agents, to draw who asked each request. */
  roster?: readonly WorkspaceAgentResponse[];
  /** The workspace's tasks, to find an agent removed from the roster mid-task. */
  loadedTasks?: () => readonly WorkspaceTaskResponse[];
}) => {
  const requester = (req: { agentId: string | null }) => requestingAgent(req.agentId, roster, loadedTasks?.() ?? []);
  const requesterName = (req: { agentId: string | null }) => requester(req)?.name ?? 'An agent';
  return (
    <PendingRequestStack
      workspaceId={workspaceId}
      source={PATH_GRANT_REQUESTS}
      className={styles.stack}
      announce={(req) =>
        `${requesterName(req)} wants ${req.requestedAccess === 'read_write' ? 'read and write' : 'read'} access to ${req.requestedPath}.`
      }
      renderCard={(req, card) => <PathGrantCard {...card} req={req} requester={requester(req)} />}
    />
  );
};

export default InlinePathGrantCard;
