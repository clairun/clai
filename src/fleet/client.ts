import { invoke } from '@tauri-apps/api/core';
import type { FleetWorkspace } from '../generated/bindings';

/** The `limit` most recently active workspaces, most recent first, plus `keep` however old. Read-only. */
export async function fleetSnapshot(limit: number, keep: string | null): Promise<FleetWorkspace[]> {
  return invoke('fleet_snapshot', { limit, keep });
}
