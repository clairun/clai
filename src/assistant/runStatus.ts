import type { AssistantRun, RunStatus } from '../generated/bindings';

export const ACTIVE_RUN_STATUSES: readonly RunStatus[] = [
  'queued',
  'running',
  'waiting_for_tool',
];

export const hasActiveAssistantRun = (runs: AssistantRun[] | undefined | null): boolean =>
  (runs || []).some((run) => ACTIVE_RUN_STATUSES.includes(run.status));
