/**
 * Craft: the files a shared agent writes about how to do its work, and that
 * CLAI lists in its prompt in every workspace. A drill-in view of the agent
 * form: the files on the left, the open one on the right. The agent writes
 * them; here the user reads, corrects and deletes them.
 */

import React, { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { deleteAgentCraft, listAgentCraft, readAgentCraft, writeAgentCraft } from '../../api/client';
import { askOnce } from '../../utils/askOnce';
import type { AgentCraftFile } from '../../generated/bindings';
import form from './SettingsForm.module.css';
import styles from './AgentCraft.module.css';

interface OpenFile {
  path: string;
  /** The content on disk, as last read or saved. */
  saved: string;
  text: string;
}

type Busy = 'reading' | 'saving' | 'deleting' | null;

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

export const CraftView = ({ definitionId, headingRef, onBack, context }: {
  definitionId: string;
  headingRef: React.RefObject<HTMLHeadingElement | null>;
  onBack: () => void;
  context: string;
}) => {
  const [files, setFiles] = useState<AgentCraftFile[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [open, setOpen] = useState<OpenFile | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // The file whose row takes focus once the editor closes.
  const returnFocusTo = useRef<string | null>(null);

  const refresh = useCallback(
    () =>
      listAgentCraft(definitionId).then(
        (next) => {
          setFiles(next);
          setListError(null);
        },
        (err) => setListError(errText(err, 'Failed to list the craft files.'))
      ),
    [definitionId]
  );

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Waits for the list to settle: the rows are disabled while busy.
  useLayoutEffect(() => {
    const path = returnFocusTo.current;
    if (open || busy || path === null) return;
    returnFocusTo.current = null;
    const rows = Array.from(listRef.current?.querySelectorAll<HTMLElement>('[data-path]') ?? []);
    (rows.find((row) => row.dataset.path === path) ?? rows[0] ?? headingRef.current)?.focus();
  }, [open, busy, headingRef]);

  const mayDiscard = async () =>
    !open ||
    open.text === open.saved ||
    askOnce(`Discard your changes to ${open.path}?`, {
      title: 'Discard changes?',
      kind: 'warning',
      okLabel: 'Discard',
      cancelLabel: 'Keep editing',
    });

  const run = async (kind: Busy, action: () => Promise<void>) => {
    setBusy(kind);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(errText(err, 'Something went wrong.'));
    } finally {
      setBusy(null);
      // The action disabled its own button, which drops focus to the page,
      // where Escape would close Settings. When the file closed instead, the
      // focus effect above moves on to the list.
      textareaRef.current?.focus();
    }
  };

  const openFile = async (path: string) => {
    if (open?.path === path || !(await mayDiscard())) return;
    void run('reading', async () => {
      const content = await readAgentCraft(definitionId, path);
      setOpen({ path, saved: content, text: content });
    });
  };

  const close = async () => {
    if (!open || !(await mayDiscard())) return;
    returnFocusTo.current = open.path;
    setOpen(null);
    setError(null);
  };

  const save = (file: OpenFile) =>
    run('saving', async () => {
      await writeAgentCraft(definitionId, file.path, file.text);
      setOpen({ ...file, saved: file.text });
      // The description and the order may have changed with the frontmatter.
      await refresh();
    });

  const remove = async (file: OpenFile) => {
    const confirmed = await askOnce(`Delete ${file.path}? The agent stops seeing this lesson. This cannot be undone.`, {
      title: 'Delete craft file?',
      kind: 'warning',
      okLabel: 'Delete',
      cancelLabel: 'Cancel',
    });
    if (!confirmed) return;
    void run('deleting', async () => {
      await deleteAgentCraft(definitionId, file.path);
      await refresh();
      returnFocusTo.current = file.path;
      setOpen(null);
    });
  };

  const leave = async () => {
    if (await mayDiscard()) onBack();
  };

  // Escape closes the open file before it can leave the view.
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== 'Escape' || !open) return;
    e.stopPropagation();
    if (!busy) void close();
  };

  // tabIndex -1 keeps focus here after a click on blank space, so Escape
  // reaches handleKeyDown and its guard rather than the host's.
  return (
    <section className={`${form.panel} ${form.panelFill}`} onKeyDown={handleKeyDown} tabIndex={-1}>
      <header className={form.panelHeader}>
        <button type="button" className={form.quietButton} onClick={() => void leave()} aria-label={`Back to ${context}`}>
          ‹ {context}
        </button>
        <h3 className={form.panelTitle} ref={headingRef} tabIndex={-1}>Craft</h3>
        <span className={form.panelMeta}>What this agent has learnt, in every workspace</span>
      </header>
      <div className={form.panelBody}>
        {error && <p className={form.errorBanner} role="alert">{error}</p>}
        {listError && <p className={form.errorBanner} role="alert">{listError}</p>}
        {files === null ? (
          !listError && (
            <div className={form.skeleton} aria-busy="true" aria-label="Loading craft">
              <span /><span /><span />
            </div>
          )
        ) : files.length === 0 ? (
          <NoCraft />
        ) : (
          <div className={styles.split}>
            <CraftList
              ref={listRef}
              files={files}
              openPath={open?.path ?? null}
              onOpen={(path) => void openFile(path)}
              disabled={busy !== null}
            />
            {open ? (
              <CraftEditor
                file={open}
                textareaRef={textareaRef}
                valid={files.find((f) => f.path === open.path)?.frontmatterValid ?? true}
                busy={busy}
                onChange={(text) => setOpen({ ...open, text })}
                onSave={() => void save(open)}
                onClose={() => void close()}
                onDelete={() => void remove(open)}
              />
            ) : (
              <p className={`${form.hint} ${styles.placeholder}`}>
                {busy === 'reading' ? 'Opening…' : 'Choose a file to read or edit it.'}
              </p>
            )}
          </div>
        )}
      </div>
    </section>
  );
};

const NoCraft = () => (
  <div className={styles.empty} role="status">
    <p className={form.lede}>
      Nothing here yet. As this agent works, it writes down what it learns about doing its work, and it uses
      those notes in every workspace that assigns it.
    </p>
  </div>
);

const CraftList = ({ ref, files, openPath, onOpen, disabled }: {
  ref: React.Ref<HTMLUListElement>;
  files: AgentCraftFile[];
  openPath: string | null;
  onOpen: (path: string) => void;
  disabled: boolean;
}) => (
  <ul className={styles.files} ref={ref} aria-label="Craft files">
    {files.map((file) => (
      <li key={file.path} className={styles.fileItem}>
        <button
          type="button"
          className={styles.file}
          data-path={file.path}
          aria-current={file.path === openPath ? 'true' : undefined}
          onClick={() => onOpen(file.path)}
          disabled={disabled}
        >
          <span className={styles.filePath}>{file.path}</span>
          {file.updatedAt && <span className={styles.fileDate}>{file.updatedAt}</span>}
          <span className={styles.fileDescription}>{file.description ?? 'No description.'}</span>
          {!file.frontmatterValid && <span className={`${form.tag} ${styles.fixTag}`}>Fix frontmatter</span>}
        </button>
      </li>
    ))}
  </ul>
);

const CraftEditor = ({ file, textareaRef, valid, busy, onChange, onSave, onClose, onDelete }: {
  file: OpenFile;
  textareaRef: React.Ref<HTMLTextAreaElement>;
  valid: boolean;
  busy: Busy;
  onChange: (text: string) => void;
  onSave: () => void;
  onClose: () => void;
  onDelete: () => void;
}) => {
  const ids = useId();
  const dirty = file.text !== file.saved;
  return (
    <div className={styles.editor}>
      <div className={styles.editorBar}>
        <h4 className={styles.editorPath} id={`${ids}-path`}>{file.path}</h4>
        {dirty && busy !== 'saving' && <span className={styles.dirty}>Unsaved changes</span>}
        <span className={form.panelActions}>
          <button type="button" className={form.dangerButton} onClick={onDelete} disabled={busy !== null}>
            {busy === 'deleting' ? 'Deleting…' : 'Delete'}
          </button>
          <button type="button" className={form.secondaryButton} onClick={onClose} disabled={busy !== null}>
            Close
          </button>
          <button type="button" className={form.primaryButton} onClick={onSave} disabled={busy !== null || !dirty}>
            {busy === 'saving' ? 'Saving…' : 'Save'}
          </button>
        </span>
      </div>
      {!valid && (
        <p className={form.hintWarning} id={`${ids}-fix`}>
          The file must start with a frontmatter block: a <code className={form.code}>---</code> line,
          a <code className={form.code}>description:</code> saying when to read it,
          an <code className={form.code}>updated_at:</code> date (YYYY-MM-DD), and a closing{' '}
          <code className={form.code}>---</code> line.
        </p>
      )}
      <textarea
        key={file.path}
        ref={textareaRef}
        className={`${form.textarea} ${form.textareaFill} ${form.mono}`}
        aria-labelledby={`${ids}-path`}
        aria-describedby={valid ? undefined : `${ids}-fix`}
        value={file.text}
        onChange={(e) => onChange(e.target.value)}
        readOnly={busy !== null}
        spellCheck={false}
        // The user just chose this file: take them to it.
        autoFocus
      />
    </div>
  );
};
