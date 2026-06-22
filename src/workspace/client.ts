/**
 * Workspace Client
 *
 * Thin wrapper around Tauri invoke calls for workspace operations.
 */

import { invoke } from '@tauri-apps/api/core';
import type {
  ContentPart,
  WorkspaceAgentResponse,
  WorkspaceDirEntry,
  WorkspaceFileBytes,
  WorkspaceFileContent,
  WorkspaceFileEntry,
  WorkspaceListEntry,
  WorkspaceSessionBinding,
  WorkspaceSnapshot,
} from '../generated/bindings';

interface SnapshotOptions {
  includeSessionPayload?: boolean;
  includeFiles?: boolean;
}

export async function getWorkspaceSnapshot(
  workspaceId: string = 'default',
  options: SnapshotOptions | null = null
): Promise<WorkspaceSnapshot> {
  return invoke('workspace_get_snapshot', { workspaceId, options });
}

export async function getOrCreateWorkspaceSession(
  workspaceId: string = 'default'
): Promise<WorkspaceSessionBinding> {
  return invoke('workspace_get_or_create_session', { workspaceId });
}

/**
 * List a single directory level of the artifact tree. Called lazily by the
 * artifacts panel — once for the root, then per folder as the user expands it —
 * so a workspace with tens of thousands of artifacts never has to be walked or
 * held in memory all at once. `path` is relative to the workspace root; omit it
 * (or pass '') for the root level.
 */
export async function listWorkspaceDir(
  workspaceId: string,
  path?: string
): Promise<WorkspaceDirEntry[]> {
  return invoke('workspace_list_dir', {
    request: { workspaceId, path: path || null },
  });
}

/**
 * Search the entire artifact tree server-side for files whose relative path
 * matches the query (case-insensitive). Needed because the panel only lazy-
 * loads the directory levels the user has expanded, so client-side filtering
 * could never span unopened folders.
 */
export async function searchWorkspaceArtifacts(
  workspaceId: string,
  query: string
): Promise<WorkspaceFileEntry[]> {
  return invoke('workspace_search_artifacts', {
    request: { workspaceId, query },
  });
}

export async function readWorkspaceFile(
  workspaceId: string,
  path: string
): Promise<WorkspaceFileContent> {
  return invoke('workspace_read_file', {
    request: { workspaceId, path },
  });
}

/**
 * Read a workspace file as base64-encoded bytes plus a best-effort MIME
 * type. Used by the HTML-preview bundler to inline local resources
 * (stylesheets, scripts, images, fonts) that a report references by
 * relative path. Resolution is confined to the workspace root server-side.
 */
export async function readWorkspaceFileBase64(
  workspaceId: string,
  path: string
): Promise<WorkspaceFileBytes> {
  return invoke('workspace_read_file_base64', {
    request: { workspaceId, path },
  });
}

/**
 * Persist a pasted/attached image under the workspace's image store and return
 * a ready-to-attach `ContentPart` (image) referencing the stored file.
 * `dataBase64` must be the raw base64 bytes (no `data:` URL prefix).
 */
export async function storeWorkspaceImage(
  workspaceId: string,
  dataBase64: string,
  mediaType: string,
  filename: string | null = null,
): Promise<ContentPart> {
  return invoke('workspace_store_image', {
    request: { workspaceId, dataBase64, mediaType, filename },
  });
}

export async function pickAndStoreWorkspaceImage(
  workspaceId: string,
): Promise<ContentPart | null> {
  // The native file dialog runs in the backend so the chosen path never
  // crosses the renderer trust boundary. Returns null if the user cancels.
  return invoke('workspace_pick_and_store_image', { workspaceId });
}

export async function writeWorkspaceFile(
  workspaceId: string,
  path: string,
  content: string
): Promise<string> {
  return invoke('workspace_write_file', {
    request: { workspaceId, path, content },
  });
}

export async function downloadWorkspaceFile(
  workspaceId: string,
  path: string,
  destination: string
): Promise<string> {
  return invoke('workspace_download_file', {
    request: { workspaceId, path, destination },
  });
}

/** Open a workspace file/folder in the configured editor, system app, or
 *  terminal. `relPath` omitted/empty targets the workspace root. */
export async function openWorkspacePath(
  workspaceId: string,
  relPath: string | null,
  target: 'editor' | 'system' | 'terminal'
): Promise<void> {
  return invoke('open_workspace_path', { workspaceId, relPath, target });
}

/** Copy user-picked files (absolute host paths from the native dialog)
 *  into the workspace root. Returns the created file names. */
export async function importWorkspaceFiles(
  workspaceId: string,
  sourcePaths: string[]
): Promise<string[]> {
  return invoke('workspace_import_files', { workspaceId, sourcePaths, destRelPath: null });
}

export async function updateWorkspaceSessionMcp(
  workspaceId: string,
  mcpServerIds: string[]
): Promise<void> {
  return invoke('workspace_update_session_mcp', {
    request: { workspaceId, mcpServerIds },
  });
}

export async function setWorkspaceProvider(
  workspaceId: string,
  providerConnectionId: string
): Promise<void> {
  return invoke('workspace_set_provider', { workspaceId, providerConnectionId });
}

export async function listWorkspaceAgents(workspaceId: string): Promise<WorkspaceAgentResponse[]> {
  return invoke('workspace_list_agents', { workspaceId });
}

// assignWorkspaceAgent / unassignWorkspaceAgent: removed. Agents are
// workspace-local; use workspaceCreateAgent / workspaceDeleteAgent from
// `../api/client.js` instead.

export async function setWorkspaceDefaultAgent(
  workspaceId: string,
  workspaceAgentId: string
): Promise<void> {
  return invoke('workspace_set_default_agent', { workspaceId, workspaceAgentId });
}

export async function acknowledgeWorkspaceTask(workspaceId: string, taskId: string): Promise<void> {
  return invoke('workspace_acknowledge_task', {
    request: { workspaceId, taskId },
  });
}

export async function createWorkspace(title?: string | null): Promise<string> {
  return invoke('workspace_create', { title: title || null });
}

/**
 * Fork a workspace's durable setup and files into a new workspace. Runtime
 * state (runs/tasks/queued approvals) starts fresh. Returns the new workspace id.
 */
export async function forkWorkspace(
  workspaceId: string,
  prompt?: string | null,
  title?: string | null,
): Promise<string> {
  return invoke('workspace_fork', {
    request: {
      workspaceId,
      prompt: prompt || null,
      title: title || null,
    },
  });
}

export async function listWorkspaces(): Promise<WorkspaceListEntry[]> {
  return invoke('workspace_list');
}

export async function deleteWorkspace(workspaceId: string): Promise<void> {
  return invoke('workspace_delete', { workspaceId });
}

export async function runWorkspaceNow(workspaceId: string): Promise<void> {
  return invoke('workspace_run_now', { workspaceId });
}

export async function setWorkspaceSchedulePaused(
  workspaceId: string,
  paused: boolean
): Promise<void> {
  return invoke('workspace_set_schedule_paused', { workspaceId, paused });
}

/** Whether the agent scheduler is globally paused (the "pause all" overlay). */
export async function getSchedulerPaused(): Promise<boolean> {
  return invoke('get_scheduler_paused');
}

/** Globally pause/resume the agent scheduler ("pause all workspaces"). */
export async function setSchedulerPaused(paused: boolean): Promise<void> {
  return invoke('set_scheduler_paused', { paused });
}

export async function setWorkspaceTitle(workspaceId: string, title: string): Promise<void> {
  return invoke('workspace_set_title', { workspaceId, title });
}

/**
 * Record that the user opened (is viewing) a workspace, clearing the rail's
 * "unread" indicator. Does not bump `updatedAt`, so it never reorders the
 * recency-sorted workspace list.
 */
export async function markWorkspaceOpened(workspaceId: string): Promise<void> {
  return invoke('workspace_mark_opened', { workspaceId });
}
