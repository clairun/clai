import { useId, useState, type ReactElement, type ReactNode } from 'react';
import type { LocalListenerView, LocalRunOnlyReason, LocalStartedBy } from '../generated/bindings';
import styles from './InlineApprovalCard.module.css';
import grantStyles from './InlinePathGrantCard.module.css';
import localStyles from './LocalListenerDetails.module.css';

type ProgramListener = Extract<LocalListenerView, { kind: 'program' }>;
type UserListener = Extract<LocalListenerView, { kind: 'otherUser' }>;

/** A listener “Always” can save: one program, or another user's service. */
export const isSavable = (
  listener: LocalListenerView
): listener is ProgramListener | UserListener => listener.kind !== 'runOnly';

/** What “Allow this run” lets through; for another user, any of its programs. */
export const allowRunTitle = (listener: LocalListenerView, port: number) => {
  switch (listener.kind) {
    case 'program':
      return `Allows ${listener.exe} on port ${port} until this run ends.`;
    case 'otherUser':
      return `Allows any program of user ${listener.userName} on port ${port} until this run ends.`;
    case 'runOnly':
      return `Allows what listens on port ${port} now, until this run ends.`;
  }
};

export const alwaysTitle = (listener: ProgramListener | UserListener, port: number) =>
  `Adds port ${port}, served by ${
    listener.kind === 'program' ? listener.exe : `user ${listener.userName} (uid ${listener.uid})`
  }, to this agent's allowed local ports.`;

/** “/usr/bin/ssh” or “user postgres”, as “Always” saves it. */
export const ServedBy = ({ listener }: { listener: ProgramListener | UserListener }) =>
  listener.kind === 'program' ? <code>{listener.exe}</code> : <>user {listener.userName}</>;

/**
 * Who started the listener, when CLAI did. It informs only; no grant depends
 * on it. The return type makes a new kind fail to compile until it is named.
 */
const startedByText = (startedBy: LocalStartedBy): string | ReactElement => {
  switch (startedBy.kind) {
    case 'terminal':
      return 'Started in a CLAI terminal.';
    case 'app':
      // A program name, such as `code`, which reads as a plain word without code style.
      return <>Started by <code>{startedBy.name}</code>, opened from CLAI.</>;
    case 'agent':
      return startedBy.name ? `Started by a command from agent ${startedBy.name}.` : 'Started by an agent command.';
    case 'mcpServer':
      return `Started by MCP server ${startedBy.name}.`;
  }
};

const startedByOf = (listener: LocalListenerView) =>
  listener.kind !== 'otherUser' && listener.startedBy ? startedByText(listener.startedBy) : null;

const StartedBy = ({ listener }: { listener: LocalListenerView }) => {
  const text = startedByOf(listener);
  return text ? <p className={styles.segmentHint}>{text}</p> : null;
};

/**
 * The listener for the spoken (Fleet) card, after the question. A run-only
 * listener is left to the run-only line below it.
 */
export const SpokenListener = ({ listener }: { listener: LocalListenerView }) => {
  const served = isSavable(listener) ? (
    <>
      It is served by <ServedBy listener={listener} />.
    </>
  ) : null;
  const startedBy = startedByOf(listener);
  if (!served && !startedBy) return null;
  return (
    <>
      {' '}
      <span>
        {served}
        {served && startedBy && ' '}
        {startedBy}
      </span>
    </>
  );
};

const RUN_ONLY_REASONS: Record<LocalRunOnlyReason, (port: number) => string> = {
  unseen: (port) => `CLAI cannot see every program listening on port ${port}.`,
  otherNamespace: (port) =>
    `The program on port ${port} runs in a container or another sandbox, so CLAI cannot name it.`,
  severalPrograms: (port) => `Different programs listen on port ${port}:`,
  replaced: (port) =>
    `The program on port ${port} was replaced or deleted on disk since it started.`,
  pathNotUtf8: (port) =>
    `The path of the program on port ${port} is not valid UTF-8, so CLAI cannot save it.`,
};

/** A program's command line is long when it would not fit in about three lines. */
const LONG_COMMAND_CHARS = 240;
const SHOWN_LINES = 3;

/**
 * A command line, always as text, collapsed to three lines when long. With
 * several programs listed, `pid` tells their toggles apart.
 */
const CommandLine = ({ text, pid }: { text: string; pid?: number }) => {
  const id = useId();
  const [expanded, setExpanded] = useState(false);
  if (!text) return null;
  const long = text.length > LONG_COMMAND_CHARS || text.split('\n').length > SHOWN_LINES;
  const toggle = expanded ? 'Show less' : 'Show all';
  return (
    <>
      <code
        id={id}
        className={`${styles.command} ${localStyles.commandText}`}
        data-collapsed={long && !expanded ? '' : undefined}
      >
        {text}
      </code>
      {long && (
        <button
          type="button"
          className={localStyles.toggle}
          aria-controls={id}
          aria-expanded={expanded}
          aria-label={pid === undefined ? undefined : `${toggle} of the command line of pid ${pid}`}
          onClick={() => setExpanded((e) => !e)}
        >
          {toggle}
        </button>
      )}
    </>
  );
};

const Program = ({
  exe,
  pid,
  commandLine,
  oneOfSeveral = false,
}: {
  exe: string;
  pid: number;
  commandLine: string;
  oneOfSeveral?: boolean;
}) => (
  <>
    <p className={styles.segmentHint}>
      Listening: <code>{exe}</code> (pid {pid})
    </p>
    <CommandLine text={commandLine} pid={oneOfSeveral ? pid : undefined} />
  </>
);

const Note = ({ children }: { children: ReactNode }) => (
  <p className={`${grantStyles.warning} ${grantStyles.warningCaution}`}>{children}</p>
);

/** What listens on a port of this computer, as the full card shows it. */
export const LocalListenerDetails = ({
  listener,
  port,
}: {
  listener: LocalListenerView;
  port: number;
}) => {
  switch (listener.kind) {
    case 'program':
      return (
        <div className={localStyles.details}>
          <Program exe={listener.exe} pid={listener.pid} commandLine={listener.commandLine} />
          <StartedBy listener={listener} />
          {listener.interpreter && (
            <Note>
              <code>{listener.exe}</code> runs many programs, so allowing it allows any of them on
              port {port}.
            </Note>
          )}
        </div>
      );
    case 'otherUser':
      return (
        <div className={localStyles.details}>
          <p className={styles.segmentHint}>
            Listening: a program of user <strong>{listener.userName}</strong> (uid {listener.uid}).
            CLAI cannot see which program, because it runs as another user.
          </p>
          <Note>
            Any program of user {listener.userName} on port {port} will be allowed, for this run or
            always.
          </Note>
        </div>
      );
    case 'runOnly':
      return (
        <div className={localStyles.details}>
          <p className={styles.segmentHint}>{RUN_ONLY_REASONS[listener.reason](port)}</p>
          {listener.holders.length > 0 && (
            <ul className={localStyles.holders}>
              {listener.holders.map((holder, i) => (
                <li key={`${holder.pid}-${i}`} className={localStyles.details}>
                  <Program
                    exe={holder.exe}
                    pid={holder.pid}
                    commandLine={holder.commandLine}
                    oneOfSeveral={listener.holders.length > 1}
                  />
                </li>
              ))}
            </ul>
          )}
          <StartedBy listener={listener} />
        </div>
      );
  }
};
