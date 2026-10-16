/**
 * The workspace-level pages of the settings window: General and Schedule.
 * Both are drafts saved through the window's save bar (`SectionHandle`);
 * Pause/Resume and Run now act at once, because they are commands rather
 * than settings.
 */

import React, { useCallback, useEffect, useImperativeHandle, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { setWorkspaceTitle } from '../../workspace/client';
import IntervalSelect from './IntervalSelect';
import { Switch } from './ListEditors';
import { useReportDirty, type SectionHandle } from './sectionHandle';
import type { ScheduleKind, WorkspaceDetails } from '../../generated/bindings';
import form from './SettingsForm.module.css';
import styles from './WorkspacePages.module.css';

interface PageProps {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  details: WorkspaceDetails | null;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
}

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

// ── General ──────────────────────────────────────────────────────────

export const GeneralPage = ({ ref, workspaceId, details, saving, onDirtyChange }: PageProps) => {
  const [title, setTitle] = useState(details?.title || '');
  const [error, setError] = useState<string | null>(null);
  const [idCopied, setIdCopied] = useState(false);

  // The draft follows the saved title when it changes underneath (a save
  // landed, or a rename elsewhere). A refetch returning the same title
  // does not re-run this, so an in-flight edit survives it.
  // eslint-disable-next-line react-hooks/set-state-in-effect -- the draft mirrors details.title; the dependency is what keeps a same-value refetch from clobbering it.
  useEffect(() => { setTitle(details?.title || ''); }, [details?.title]);

  useReportDirty(title.trim() !== (details?.title || '').trim(), onDirtyChange);

  const workspaceUuid = details?.workspaceId;
  const copyId = async () => {
    if (!workspaceUuid) return;
    try {
      await navigator.clipboard.writeText(workspaceUuid);
      setIdCopied(true);
      window.setTimeout(() => setIdCopied(false), 1200);
    } catch {
      // The id stays selectable as text.
    }
  };

  const check = (): string | null => {
    const trimmed = title.trim();
    if (!trimmed) return 'Workspace title cannot be empty.';
    if (trimmed.length > 100) return 'Workspace title must be 100 characters or less.';
    return null;
  };

  useImperativeHandle(ref, () => ({
    validate: () => {
      const problem = check();
      setError(problem);
      return problem ? { ok: false, error: problem } : { ok: true };
    },
    submit: async () => {
      try {
        await setWorkspaceTitle(workspaceId, title.trim());
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = errText(err, 'Failed to save title.');
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  return (
    <div className={form.page}>
      <div className={styles.narrow}>
        <div className={form.field}>
          <label className={form.label} htmlFor="ws-title">
            Title <span className={form.required} aria-hidden="true">*</span>
          </label>
          <input
            id="ws-title"
            type="text"
            className={form.input}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            disabled={saving}
            maxLength={100}
            aria-invalid={!!error}
          />
          <p className={form.hint}>Shown in the Fleet view and at the top of this workspace.</p>
        </div>

        <dl className={styles.facts}>
          <dt>Workspace ID</dt>
          <dd>
            <button
              type="button"
              className={`${styles.copyId} ${idCopied ? styles.copyIdDone : ''}`}
              onClick={copyId}
              disabled={!details?.workspaceId}
              aria-label={idCopied ? 'Workspace ID copied' : 'Copy workspace ID'}
              title={idCopied ? 'Copied' : 'Click to copy'}
            >
              <code>{details?.workspaceId}</code>
              <span className={styles.copyIdState} aria-hidden="true">{idCopied ? 'Copied' : 'Copy'}</span>
            </button>
          </dd>
          {details?.rootPath && (
            <>
              <dt>Folder</dt>
              <dd><code className={form.code}>{details.rootPath}</code></dd>
            </>
          )}
        </dl>

        {error && <p className={form.errorBanner} role="alert">{error}</p>}
      </div>
    </div>
  );
};

// ── Schedule ─────────────────────────────────────────────────────────

type ScheduleDraft =
  | { type: 'interval'; intervalMinutes: number }
  | { type: 'cron'; expression: string; timezone: string };

// One click for the common cases; the expression stays editable below.
const CRON_PRESETS = [
  { label: 'Hourly', value: '0 * * * *' },
  { label: 'Daily 9am', value: '0 9 * * *' },
  { label: 'Daily midnight', value: '0 0 * * *' },
  { label: 'Weekdays 9am', value: '0 9 * * 1-5' },
  { label: 'Mondays 9am', value: '0 9 * * 1' },
  { label: 'Monthly', value: '0 0 1 * *' },
];

const draftFrom = (kind: ScheduleKind | null | undefined): ScheduleDraft => {
  if (kind?.type === 'cron') {
    return { type: 'cron', expression: kind.expression || '', timezone: kind.timezone || '' };
  }
  if (kind?.type === 'interval') {
    return { type: 'interval', intervalMinutes: kind.intervalMinutes ?? 30 };
  }
  return { type: 'interval', intervalMinutes: 30 };
};

const formatAbsolute = (ms: number): string => {
  const d = new Date(ms);
  const sameYear = d.getFullYear() === new Date().getFullYear();
  return d.toLocaleString(undefined, {
    year: sameYear ? undefined : 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

// "in 3h" / "in 2d"; past 30 days the absolute date says enough.
const formatRelative = (ms: number): string => {
  const delta = ms - Date.now();
  if (!Number.isFinite(delta) || delta <= 0) return 'now';
  const min = Math.round(delta / 60_000);
  if (min < 60) return `in ${min}m`;
  const hr = Math.round(min / 60);
  if (hr < 48) return `in ${hr}h`;
  const day = Math.round(hr / 24);
  return day < 30 ? `in ${day}d` : '';
};

export const SchedulePage = ({
  ref,
  workspaceId,
  details,
  saving,
  onDirtyChange,
  onDetailsRefresh,
}: PageProps & { onDetailsRefresh?: () => void }) => {
  const [enabled, setEnabled] = useState(!!details?.scheduleEnabled);
  const [draft, setDraft] = useState(() => draftFrom(details?.scheduleKind));
  // Pause/Resume and Run now are immediate commands with their own busy state.
  const [commandBusy, setCommandBusy] = useState(false);
  const busy = saving || commandBusy;
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<number[]>([]);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [hostTimezone, setHostTimezone] = useState('UTC');

  useEffect(() => {
    let cancelled = false;
    invoke('workspace_host_timezone')
      .then((tz) => {
        if (!cancelled && typeof tz === 'string' && tz) setHostTimezone(tz);
      })
      .catch(() => {
        const fallback = Intl.DateTimeFormat().resolvedOptions().timeZone;
        if (!cancelled && fallback) setHostTimezone(fallback);
      });
    return () => { cancelled = true; };
  }, []);

  // Keyed on the schedule's value, not its object: the workspace re-fetches
  // details every 5 s, and a same-value refetch must not discard the draft.
  const savedKind = JSON.stringify(details?.scheduleKind ?? null);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- resyncs the two-field draft when the saved schedule changes underneath.
    setEnabled(!!details?.scheduleEnabled);
    setDraft(draftFrom(JSON.parse(savedKind) as ScheduleKind | null));
  }, [details?.scheduleEnabled, savedKind]);

  // Next fire times for the cron being typed, so it can be checked before Save.
  useEffect(() => {
    const expression = draft.type === 'cron' ? draft.expression.trim() : '';
    const timezone = draft.type === 'cron' ? draft.timezone.trim() : '';
    if (!enabled || !expression || !timezone) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- clears the preview when there is nothing to preview; the fetch below is async.
      setPreview([]);
      setPreviewError(null);
      return undefined;
    }
    let cancelled = false;
    invoke('workspace_preview_schedule', { kind: { type: 'cron', expression, timezone }, count: 3 })
      .then((times) => {
        if (cancelled) return;
        setPreview(Array.isArray(times) ? times : []);
        setPreviewError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        setPreview([]);
        setPreviewError(errText(err, 'Invalid schedule.'));
      });
    return () => { cancelled = true; };
  }, [enabled, draft]);

  const paused = !!details?.schedulePaused;
  const savedOn = !!details?.scheduleEnabled;

  useReportDirty(
    enabled !== savedOn || JSON.stringify(draft) !== JSON.stringify(draftFrom(details?.scheduleKind)),
    onDirtyChange
  );

  const setType = (type: 'interval' | 'cron') => {
    setDraft((prev) => {
      if (prev.type === type) return prev;
      return type === 'cron'
        ? { type: 'cron', expression: '0 * * * *', timezone: hostTimezone || 'UTC' }
        : { type: 'interval', intervalMinutes: 30 };
    });
  };

  const build = useCallback((): { ok: true; kind: ScheduleKind | null } | { ok: false; error: string } => {
    if (!enabled) return { ok: true, kind: null };
    if (draft.type === 'interval') {
      const mins = Number(draft.intervalMinutes);
      if (!Number.isFinite(mins) || mins < 1 || mins > 1440) {
        return { ok: false, error: 'Interval must be between 1 minute and 24 hours.' };
      }
      return { ok: true, kind: { type: 'interval', intervalMinutes: mins } as ScheduleKind };
    }
    const expression = draft.expression.trim();
    const timezone = draft.timezone.trim();
    if (!expression) return { ok: false, error: 'Cron expression is required.' };
    if (!timezone) return { ok: false, error: 'Timezone is required.' };
    return { ok: true, kind: { type: 'cron', expression, timezone } as ScheduleKind };
  }, [enabled, draft]);

  const runCommand = useCallback(async (call: () => Promise<unknown>, fallback: string) => {
    setCommandBusy(true);
    setError(null);
    try {
      await call();
      onDetailsRefresh?.();
    } catch (err) {
      setError(errText(err, fallback));
    } finally {
      setCommandBusy(false);
    }
  }, [onDetailsRefresh]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const built = build();
      setError(built.ok ? null : built.error);
      return built.ok ? { ok: true } : { ok: false, error: built.error };
    },
    submit: async () => {
      const built = build();
      if (!built.ok) {
        setError(built.error);
        return { ok: false, error: built.error };
      }
      try {
        await invoke('workspace_set_schedule', { workspaceId, kind: built.kind });
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = errText(err, 'Failed to save schedule.');
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  const off = busy || !enabled;

  return (
    <div className={form.page}>
      <div className={form.columns}>
        <section className={form.panel}>
          <header className={form.panelHeader}>
            <h3 id="schedule-when" className={form.panelTitle}>When the main agent runs</h3>
          </header>
          <div className={form.panelBody}>
            <Switch
              label="Run on a recurring schedule"
              hint="Crew members don't have schedules of their own. The main agent hands them work when it needs to."
              checked={enabled}
              onChange={setEnabled}
              disabled={busy}
            />

            <div className={form.segmented} role="group" aria-label="Schedule type">
              {(['interval', 'cron'] as const).map((type) => (
                <button
                  key={type}
                  type="button"
                  className={form.segment}
                  aria-pressed={draft.type === type}
                  onClick={() => setType(type)}
                  disabled={off}
                >
                  {type === 'interval' ? 'Interval' : 'Cron'}
                </button>
              ))}
            </div>

            {draft.type === 'interval' ? (
              <div className={form.field}>
                <label className={form.label} htmlFor="ws-interval">Every</label>
                <IntervalSelect
                  id="ws-interval"
                  value={draft.intervalMinutes}
                  onChange={(v) => setDraft({ type: 'interval', intervalMinutes: Number(v) })}
                  disabled={off}
                />
                <p className={form.hint}>
                  Counted from when the previous run finishes. For fixed times of day, use Cron.
                </p>
              </div>
            ) : (
              <>
                <div className={styles.presets} role="group" aria-label="Common schedules">
                  {CRON_PRESETS.map((preset) => (
                    <button
                      key={preset.value}
                      type="button"
                      className={styles.preset}
                      aria-pressed={draft.expression === preset.value}
                      onClick={() => setDraft({ ...draft, expression: preset.value })}
                      disabled={off}
                      title={preset.value}
                    >
                      {preset.label}
                    </button>
                  ))}
                </div>
                <div className={form.field}>
                  <label className={form.label} htmlFor="ws-cron-expr">Cron expression</label>
                  <input
                    id="ws-cron-expr"
                    type="text"
                    className={`${form.input} ${form.mono}`}
                    value={draft.expression}
                    onChange={(e) => setDraft({ ...draft, expression: e.target.value })}
                    placeholder="0 9 * * 1-5"
                    disabled={off}
                    spellCheck={false}
                    autoCorrect="off"
                    autoCapitalize="off"
                  />
                  <p className={form.hint}>minute · hour · day of month · month · day of week</p>
                </div>
                <div className={form.field}>
                  <label className={form.label} htmlFor="ws-cron-tz">Timezone</label>
                  <input
                    id="ws-cron-tz"
                    type="text"
                    className={form.input}
                    value={draft.timezone}
                    onChange={(e) => setDraft({ ...draft, timezone: e.target.value })}
                    placeholder="Europe/Madrid"
                    disabled={off}
                    spellCheck={false}
                    autoCorrect="off"
                    autoCapitalize="off"
                  />
                  {draft.timezone !== hostTimezone && (
                    <p className={form.hint}>
                      <button
                        type="button"
                        className={form.linkButton}
                        onClick={() => setDraft({ ...draft, timezone: hostTimezone })}
                        disabled={off}
                      >
                        Use system timezone ({hostTimezone})
                      </button>
                    </p>
                  )}
                </div>
              </>
            )}
          </div>
        </section>

        <div className={form.column}>
          <section className={form.panel}>
            <header className={form.panelHeader}>
              <h3 id="schedule-status" className={form.panelTitle}>Status</h3>
            </header>
            <div className={form.panelBody}>
              {savedOn ? (
                <div className={styles.status}>
                  <span className={styles.statusLabel}>
                    <span
                      className={`${styles.dot} ${paused ? styles.dotPaused : styles.dotRunning}`}
                      aria-hidden="true"
                    />
                    {paused ? 'Paused' : 'Running'}
                  </span>
                  <span className={styles.statusActions}>
                    {!paused && (
                      <button
                        type="button"
                        className={form.secondaryButton}
                        onClick={() => runCommand(() => invoke('workspace_run_now', { workspaceId }), 'Failed to trigger run.')}
                        disabled={busy}
                      >
                        Run now
                      </button>
                    )}
                    <button
                      type="button"
                      className={form.secondaryButton}
                      onClick={() => runCommand(
                        () => invoke('workspace_set_schedule_paused', { workspaceId, paused: !paused }),
                        'Failed to update pause state.'
                      )}
                      disabled={busy}
                    >
                      {paused ? 'Resume' : 'Pause'}
                    </button>
                  </span>
                </div>
              ) : (
                <p className={form.lede}>
                  Not scheduled. The main agent runs when you message it.
                </p>
              )}
            </div>
          </section>

          {draft.type === 'cron' && enabled && (
            <section className={form.panel}>
              <header className={form.panelHeader}>
                <h3 id="schedule-next" className={form.panelTitle}>Next runs</h3>
              </header>
              <div className={form.panelBody}>
                {previewError ? (
                  <p className={form.errorBanner}>{previewError}</p>
                ) : preview.length === 0 ? (
                  <p className={form.lede}>Enter an expression and a timezone to see the next runs.</p>
                ) : (
                  <ol className={styles.preview}>
                    {preview.map((ms) => (
                      <li key={ms}>
                        <span>{formatAbsolute(ms)}</span>
                        <span className={styles.previewRelative}>{formatRelative(ms)}</span>
                      </li>
                    ))}
                  </ol>
                )}
              </div>
            </section>
          )}

          {error && <p className={form.errorBanner} role="alert">{error}</p>}
        </div>
      </div>
    </div>
  );
};
