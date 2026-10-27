import type { AssistantRun } from '../generated/bindings';

// Mirrors the backend `is_usage_limit_error` classifier (local_agent.rs).
const isUsageLimitError = (message: string): boolean => {
  const m = message.toLowerCase();
  return (
    m.includes('usage limit') ||
    m.includes('session limit') ||
    m.includes('rate limit') ||
    m.includes('rate_limit') ||
    m.includes('quota') ||
    (m.includes("you've hit your") && m.includes('limit'))
  );
};

export interface RunFailure {
  text: string;
  /** A usage/rate limit: it resolves on its own at the reset time the text states. */
  isLimit: boolean;
}

/** What to tell the user about a run, or null unless it failed. */
export const runFailure = (run: AssistantRun | null | undefined): RunFailure | null => {
  if (run?.status !== 'failed') return null;
  const text = run.error?.trim() || 'The run failed.';
  return { text, isLimit: isUsageLimitError(text) };
};
