/**
 * "Pick a face" for a shared agent: the face it has now (when editing), six
 * fresh candidates in six different hues, and Shuffle for six more.
 *
 * The parent owns the choice so it can reset it per agent and read the seed
 * at save time; this file keeps the rules for what a choice *means* next to
 * the row that renders it. A choice never names a face directly: `picked` is
 * `null` for the default, which is the stored face when editing and the first
 * candidate when creating. That way "keep the current face" and "nothing
 * touched yet" are the same state and a save can omit `avatar` for both.
 */
import { useId, useMemo } from 'react';
import AgentAvatar from './AgentAvatar';
import { GENERATOR_VERSION, candidateSeeds, freshNonce, type AgentIdentity } from './agentIdentity';
import styles from './AgentFacePicker.module.css';

export interface FaceChoice {
  /** Seeds the candidate row; a new nonce is a new row. */
  nonce: string;
  /** A candidate the user clicked, or `null` for the default. */
  picked: string | null;
}

export const freshFaceChoice = (): FaceChoice => ({ nonce: freshNonce(), picked: null });

export const candidatesOf = (choice: FaceChoice): string[] => candidateSeeds(choice.nonce);

/**
 * The seed to save, or `null` to keep `current` — the face the agent already
 * has. `null` is only ever returned when there is one.
 */
export const chosenSeed = (choice: FaceChoice, current: AgentIdentity | null): string | null => {
  if (choice.picked !== null) return choice.picked;
  if (current) return null;
  // `candidateSeeds` yields at least one seed: `HUES` is never empty.
  return candidatesOf(choice)[0] as string;
};

/**
 * A new row. Someone who had moved off the current face lands on the first
 * new candidate rather than being bounced back to the face they left.
 */
export const shuffled = (choice: FaceChoice, current: AgentIdentity | null): FaceChoice => {
  const nonce = freshNonce();
  // Creating: `null` already means "the first candidate". Editing while on
  // the current face: stay there.
  if (!current || choice.picked === null) return { nonce, picked: null };
  return { nonce, picked: candidateSeeds(nonce)[0] as string };
};

export interface AgentFacePickerProps {
  /** The face the agent has now; omit when creating. */
  current?: AgentIdentity | null;
  choice: FaceChoice;
  onChange: (choice: FaceChoice) => void;
  disabled?: boolean;
}

const candidateIdentity = (seed: string): AgentIdentity => ({ seed, generatorVersion: GENERATOR_VERSION });

const AgentFacePicker = ({ current = null, choice, onChange, disabled = false }: AgentFacePickerProps) => {
  const legendId = useId();
  const candidates = useMemo(() => candidateSeeds(choice.nonce), [choice.nonce]);
  const seed = chosenSeed(choice, current);
  // `chosenSeed` returns `null` only when `current` exists.
  const selected = seed === null ? (current as AgentIdentity) : candidateIdentity(seed);

  const tile = (identity: AgentIdentity, checked: boolean, label: string, onPick: () => void) => (
    <button
      key={identity.seed}
      type="button"
      role="radio"
      aria-checked={checked}
      aria-label={label}
      className={`${styles.tile} ${checked ? styles.tileChecked : ''}`}
      onClick={onPick}
      disabled={disabled}
    >
      <AgentAvatar identity={identity} size={40} />
    </button>
  );

  return (
    <div className={styles.picker}>
      <AgentAvatar identity={selected} size={64} className={styles.preview} />
      <div className={styles.controls}>
        <span className={styles.legend} id={legendId}>
          Face
        </span>
        <div className={styles.row}>
          <div className={styles.candidates} role="radiogroup" aria-labelledby={legendId}>
            {current &&
              tile(current, seed === null, 'Current face', () => onChange({ ...choice, picked: null }))}
            {candidates.map((candidate, index) =>
              tile(candidateIdentity(candidate), seed === candidate, `Face ${index + 1}`, () =>
                onChange({ ...choice, picked: candidate })
              )
            )}
          </div>
          <button
            type="button"
            className={styles.shuffle}
            onClick={() => onChange(shuffled(choice, current))}
            disabled={disabled}
          >
            Shuffle
          </button>
        </div>
        <span className={styles.hint}>
          {current
            ? 'Keep the current face or pick a new one. Shuffle for six more.'
            : 'Every agent gets a face. Pick one, or Shuffle for six more.'}
        </span>
      </div>
    </div>
  );
};

export default AgentFacePicker;
