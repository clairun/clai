/**
 * Small editable lists used by several settings pages: path grants,
 * command prefixes and host names. Each is a bounded list plus one add row; the list
 * scrolls inside its panel if it ever outgrows it, the page never does.
 */

import { useEffect, useId, useRef, useState } from 'react';
import styles from './SettingsForm.module.css';

export interface GrantLike {
  path: string;
  access: string;
}

export function PathGrantEditor<T extends GrantLike>({
  grants,
  onChange,
  make,
  describe,
  warning,
  disabled,
  label = 'Path grants',
  placeholder = '/home/user/project',
}: {
  grants: T[];
  onChange: (next: T[]) => void;
  /** Builds a new grant from the add row. */
  make: (path: string, access: 'read_only' | 'read_write') => T;
  /** Optional provenance shown next to each grant (Manual, Preset, Approved …). */
  describe?: (grant: T) => { text: string; title?: string } | null;
  /** Optional warning drawn under a grant. It informs; the grant stays. */
  warning?: (grant: T) => string | null;
  disabled?: boolean;
  /** Accessible name of the list and the add row. */
  label?: string;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState('');
  const [access, setAccess] = useState<'read_only' | 'read_write'>('read_only');

  const add = () => {
    const path = draft.trim();
    if (path && !grants.some((grant) => grant.path === path)) {
      onChange([...grants, make(path, access)]);
    }
    setDraft('');
  };

  return (
    <>
      {grants.length === 0 ? (
        <p className={styles.listEmpty}>No paths granted.</p>
      ) : (
        <ul className={styles.list} aria-label={label}>
          {grants.map((grant) => {
            const origin = describe?.(grant);
            const write = grant.access === 'read_write';
            const note = warning?.(grant);
            return (
              <li key={grant.path} className={note ? `${styles.listItem} ${styles.listItemWarned}` : styles.listItem}>
                <span className={styles.listItemText} title={grant.path}>{grant.path}</span>
                {origin && <span className={styles.tag} title={origin.title}>{origin.text}</span>}
                <span className={`${styles.tag} ${write ? styles.tagWrite : ''}`}>
                  {write ? 'Read & write' : 'Read only'}
                </span>
                <button
                  type="button"
                  className={styles.removeButton}
                  onClick={() => onChange(grants.filter((item) => item.path !== grant.path))}
                  disabled={disabled}
                  aria-label={`Remove grant for ${grant.path}`}
                >
                  ×
                </button>
                {note && <p className={`${styles.hintWarning} ${styles.listItemNote}`}>{note}</p>}
              </li>
            );
          })}
        </ul>
      )}
      <div className={styles.addRow}>
        <input
          type="text"
          className={`${styles.input} ${styles.mono}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={placeholder}
          aria-label={`New path for ${label.toLowerCase()}`}
          disabled={disabled}
          spellCheck={false}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              add();
            }
          }}
        />
        <select
          className={styles.select}
          value={access}
          onChange={(e) => setAccess(e.target.value as 'read_only' | 'read_write')}
          aria-label="Access for the new path"
          disabled={disabled}
        >
          <option value="read_only">Read only</option>
          <option value="read_write">Read &amp; write</option>
        </select>
        <button type="button" className={styles.secondaryButton} onClick={add} disabled={disabled || !draft.trim()}>
          Add
        </button>
      </div>
    </>
  );
}

// Rules match single-spaced command words, so `git  push` would never match.
const singleSpaced = (raw: string): string => raw.trim().split(/\s+/).join(' ');

export const PrefixListEditor = ({
  items,
  onChange,
  disabled,
  label,
  placeholder,
  parse = singleSpaced,
  display = (item: string) => item,
}: {
  items: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
  /** Accessible name of the list, e.g. "Ask-first command prefixes". */
  label: string;
  placeholder: string;
  /** Turns what was typed into an entry, or says why it is not one. */
  parse?: (raw: string) => string | { error: string };
  /** How an entry reads, when that differs from how it is stored. */
  display?: (item: string) => string;
}) => {
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const errorId = useId();
  const list = useRef<HTMLUListElement>(null);
  // The entry just added, scrolled into view once it arrives in `items`.
  const added = useRef<string | null>(null);

  useEffect(() => {
    if (!added.current) return;
    const item = Array.from(list.current?.children ?? []).find(
      (child) => (child as HTMLElement).dataset.item === added.current,
    );
    added.current = null;
    item?.scrollIntoView?.({ block: 'nearest' });
  }, [items]);

  const add = () => {
    if (!draft.trim()) return;
    const parsed = parse(draft);
    if (typeof parsed !== 'string') {
      setError(parsed.error);
      return;
    }
    setDraft('');
    if (!parsed) return;
    if (items.includes(parsed)) {
      setError(`${display(parsed)} is already in the list.`);
      return;
    }
    added.current = parsed;
    onChange([...items, parsed]);
  };

  return (
    <>
      {items.length === 0 ? (
        <p className={styles.listEmpty}>None.</p>
      ) : (
        <ul className={styles.list} aria-label={label} ref={list}>
          {items.map((item) => {
            // A hand edit or an older form stores an entry that does not parse.
            const parsed = parse(item);
            const invalid = typeof parsed === 'string' ? null : parsed.error;
            return (
              <li key={item} className={styles.listItem} data-item={item}>
                <span className={styles.listItemText} title={display(item) === item ? undefined : item}>
                  {display(item)}
                </span>
                {invalid && <span className={styles.hintWarning}>Not valid: {invalid}</span>}
                <button
                  type="button"
                  className={styles.removeButton}
                  onClick={() => onChange(items.filter((other) => other !== item))}
                  disabled={disabled}
                  aria-label={`Remove ${display(item)}`}
                >
                  ×
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <div className={styles.addRow}>
        <input
          type="text"
          className={`${styles.input} ${styles.mono}`}
          value={draft}
          onChange={(e) => { setDraft(e.target.value); setError(null); }}
          placeholder={placeholder}
          aria-label={`New entry for ${label.toLowerCase()}`}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          disabled={disabled}
          spellCheck={false}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              add();
            }
          }}
        />
        <button type="button" className={styles.secondaryButton} onClick={add} disabled={disabled || !draft.trim()}>
          Add
        </button>
      </div>
      {error && <p className={styles.hintWarning} role="alert" id={errorId}>{error}</p>}
    </>
  );
};

/** A labelled on/off switch: a real checkbox, drawn as a switch. */
export const Switch = ({
  label,
  hint,
  checked,
  onChange,
  disabled,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
}) => {
  const hintId = useId();
  return (
    <label className={styles.switchRow}>
      <span className={styles.switchText}>
        <span>{label}</span>
        {hint && <span id={hintId} className={styles.hint}>{hint}</span>}
      </span>
      <span className={styles.switch}>
        <input
          type="checkbox"
          role="switch"
          aria-label={label}
          aria-describedby={hint ? hintId : undefined}
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          disabled={disabled}
        />
        <span className={styles.switchTrack} aria-hidden="true" />
      </span>
    </label>
  );
};
