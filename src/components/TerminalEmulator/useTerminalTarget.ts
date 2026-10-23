import { useLocation } from 'react-router';
import { useFleetStore } from '../../fleet/fleetStore';

export interface TerminalTarget {
  /** Where free-text prompts go: the open workspace, or the Fleet's selected card. */
  workspaceId: string | null;
  surface: 'workspace' | 'fleet' | 'other';
}

const WORKSPACE_ROUTE = /^\/workspace\/([^/]+)\/?$/;

/**
 * The one place that decides the composer's target. On the Fleet with no
 * card selected the target is `null`, and a prompt creates a new workspace.
 */
export function useTerminalTarget(): TerminalTarget {
  const { pathname } = useLocation();
  const selectedId = useFleetStore((s) => s.selectedId);
  const match = pathname.match(WORKSPACE_ROUTE);
  if (match) return { workspaceId: decodeURIComponent(match[1]!), surface: 'workspace' };
  if (pathname === '/workspace') return { workspaceId: null, surface: 'workspace' };
  if (pathname === '/fleet') return { workspaceId: selectedId, surface: 'fleet' };
  return { workspaceId: null, surface: 'other' };
}
