import type { AssistantRun } from '../generated/bindings';
import { hasActiveAssistantRun } from '../assistant/runStatus';

export { ACTIVE_RUN_STATUSES } from '../assistant/runStatus';

interface WorkspaceSessionHydrationInput {
  existingSessionPresent: boolean;
  hasUnloadedUpdate: boolean;
  existingIsStreaming: boolean;
  snapshotRuns: AssistantRun[] | undefined | null;
}

export const shouldHydrateWorkspaceSession = ({
  existingSessionPresent,
  hasUnloadedUpdate,
  existingIsStreaming,
  snapshotRuns,
}: WorkspaceSessionHydrationInput): boolean => {
  if (!existingSessionPresent) return true;
  if (!hasUnloadedUpdate) return false;
  if (!existingIsStreaming) return true;
  return !hasActiveAssistantRun(snapshotRuns);
};
