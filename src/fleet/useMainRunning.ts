import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import { mainRunning } from './fleetModel';
import { useFleetStore } from './fleetStore';

/** Whether the Main of `workspaceId`'s card is working on a reply; false with no card. */
export const useMainRunning = (workspaceId: string | null | undefined): boolean => {
  const card = useFleetStore((s) => (workspaceId ? s.fleet.cards[workspaceId] : undefined));
  const runs = useFleetActivityStore((s) => (workspaceId ? s.activeRunsByWorkspace[workspaceId] : undefined));
  const tasks = useTaskStore((s) => (workspaceId ? s.tasks[workspaceId] : undefined));
  return !!card && mainRunning(card, { runs: { [card.workspaceId]: runs ?? [] }, tasks: { [card.workspaceId]: tasks ?? {} } });
};
