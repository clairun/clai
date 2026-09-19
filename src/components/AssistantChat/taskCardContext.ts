import { createContext, useContext } from 'react';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

/**
 * What the chat's task cards need from the page around them: the crew, for
 * the assignee's face and name, and how to open a task.
 *
 * It travels by context rather than by prop so the roster reaching the cards
 * does not walk through the memoized message/tool tree — a poll that changes
 * nothing re-renders nothing, and even a real crew change repaints only the
 * cards.
 *
 * The default value is the read-only case (no crew known, nothing to open):
 * a transcript rendered without a provider still draws its cards, inert.
 */
export interface TaskCardSurface {
  roster: readonly WorkspaceAgentResponse[];
  /** Open a task's own log. Omit to render cards inert. */
  onOpenTask?: (taskId: string) => void;
}

const EMPTY_ROSTER: readonly WorkspaceAgentResponse[] = [];

export const TaskCardContext = createContext<TaskCardSurface>({ roster: EMPTY_ROSTER });

export const useTaskCardSurface = (): TaskCardSurface => useContext(TaskCardContext);
