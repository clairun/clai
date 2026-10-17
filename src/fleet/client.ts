import { invoke } from '@tauri-apps/api/core';
import type { FleetWorkspace } from '../generated/bindings';

/** The `limit` most recently active workspaces, most recent first. Read-only. */
export async function fleetSnapshot(limit: number): Promise<FleetWorkspace[]> {
  return invoke('fleet_snapshot', { limit });
}
