import type { AssistantRun, FailureKind } from '../generated/bindings';

/**
 * The user's next step for each kind whose error text does not already name
 * one. The backend's lead sentence (`FailureKind::lead`) states the remedy for
 * billing and transient failures, and the compaction notice states it for
 * context-too-long ones.
 */
const ACTIONS: Partial<Record<FailureKind, string>> = {
  auth: 'Update the key in Settings → AI Provider.',
  usage_limit: 'Wait for it to reset, or raise the limit with the provider.',
  model_unavailable: 'Pick another model.',
  content_filtered: 'Rephrase the request and try again.',
};

export interface RunFailure {
  text: string;
  /** Absent for runs recorded before kinds existed and for unclassified failures. */
  kind?: FailureKind;
  action?: string;
}

/** What to tell the user about a run, or null unless it failed. */
export const runFailure = (run: AssistantRun | null | undefined): RunFailure | null => {
  if (run?.status !== 'failed') return null;
  const text = run.error?.trim() || 'The run failed.';
  const kind = run.errorKind;
  return { text, kind, action: kind && ACTIONS[kind] };
};
