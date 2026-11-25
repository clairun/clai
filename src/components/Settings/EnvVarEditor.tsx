import type { McpEnvVar } from '../../generated/bindings';
// The editor sits inside item forms that already use this stylesheet.
import styles from './McpServerFormModal.module.css';

/** One env var (or HTTP header) row in the editor. */
export interface EnvRow {
  key: string;
  value: string;
  secret: boolean;
  /** Key a secret was stored under when the form opened, or null. */
  storedKey: string | null;
  /** Last-4 identification hint for a stored secret, if the backend sent one. */
  hint: string | null;
}

/** Build editor rows from stored entries. Secret values are never returned. */
export function rowsFromEntries(
  entries:
    | ReadonlyArray<{ key: string; value?: string | null; secret: boolean; hint?: string | null }>
    | undefined
    | null
): EnvRow[] {
  return (entries || []).map((e) => ({
    key: e.key,
    value: e.secret ? '' : e.value || '',
    secret: e.secret,
    storedKey: e.secret ? e.key : null,
    hint: e.hint ?? null,
  }));
}

/** An empty value keeps the stored secret only while the row still names it. */
const keepsStoredSecret = (row: EnvRow) => row.secret && row.key.trim() === row.storedKey;

/**
 * Error for the first row that can't be saved: a key used twice, a secret
 * with no value to send or keep, or a stored secret unticked and left empty
 * (saving it would replace the secret). Rows with no key are dropped on save.
 * Null when valid.
 */
export function envRowsError(rows: EnvRow[]): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) continue;
    if (seen.has(key)) return `${key} is set more than once`;
    seen.add(key);
    if (row.value.trim()) continue;
    if (row.secret && !keepsStoredSecret(row)) {
      return `Enter a value for the secret ${key}`;
    }
    if (!row.secret && key === row.storedKey) {
      return `Enter a value for ${key}: saving it empty replaces its stored secret`;
    }
  }
  return null;
}

/**
 * Convert editor rows to request entries, dropping rows with an empty key.
 * A secret row sends its typed value, or no value to keep the stored one;
 * the secret_ref is assigned server-side.
 */
export function rowsToEnvVars(rows: EnvRow[]): McpEnvVar[] {
  return rows
    .filter((row) => row.key.trim())
    .map((row) => {
      const key = row.key.trim();
      if (row.secret) {
        const typed = row.value.trim();
        return typed ? { key, value: typed, secret: true } : { key, secret: true };
      }
      return { key, value: row.value, secret: false };
    });
}

export const EnvVarEditor = ({
  label,
  description,
  addLabel,
  rows,
  onChange,
  disabled,
}: {
  label: string;
  description?: string;
  addLabel: string;
  rows: EnvRow[];
  onChange: (rows: EnvRow[]) => void;
  disabled: boolean;
}) => {
  const update = (i: number, patch: Partial<EnvRow>) =>
    onChange(rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  const remove = (i: number) => onChange(rows.filter((_, idx) => idx !== i));
  const add = () =>
    onChange([...rows, { key: '', value: '', secret: false, storedKey: null, hint: null }]);

  return (
    <div className={styles.field}>
      <label className={styles.label}>{label}</label>
      {description && <span className={styles.hint}>{description}</span>}
      {rows.length > 0 && (
        <div className={styles.envRows}>
          {rows.map((row, i) => {
            const storedPlaceholder = keepsStoredSecret(row)
              ? row.hint
                ? `••••${row.hint} — enter to replace`
                : 'stored — enter to replace'
              : 'value';
            return (
              <div className={styles.envRow} key={i}>
                <input
                  className={`${styles.input} ${styles.envKey}`}
                  type="text"
                  value={row.key}
                  onChange={(e) => update(i, { key: e.target.value })}
                  placeholder="KEY"
                  disabled={disabled}
                  spellCheck={false}
                  autoCapitalize="off"
                  autoCorrect="off"
                />
                <input
                  className={`${styles.input} ${styles.envValue}`}
                  type={row.secret ? 'password' : 'text'}
                  value={row.value}
                  onChange={(e) => update(i, { value: e.target.value })}
                  placeholder={storedPlaceholder}
                  disabled={disabled}
                  spellCheck={false}
                  autoCapitalize="off"
                  autoCorrect="off"
                />
                <label className={styles.envSecretToggle} title="Store this value in the OS keyring">
                  <input
                    type="checkbox"
                    checked={row.secret}
                    onChange={(e) => update(i, { secret: e.target.checked })}
                    disabled={disabled}
                  />
                  secret
                </label>
                <button
                  type="button"
                  className={styles.envRemove}
                  onClick={() => remove(i)}
                  disabled={disabled}
                  aria-label={`Remove ${row.key || 'entry'}`}
                  title="Remove"
                >
                  {'×'}
                </button>
              </div>
            );
          })}
        </div>
      )}
      <button type="button" className={styles.envAdd} onClick={add} disabled={disabled}>
        + {addLabel}
      </button>
      <span className={styles.hint}>
        Secret values are stored in your OS keyring, never in config.json.
      </span>
    </div>
  );
};
