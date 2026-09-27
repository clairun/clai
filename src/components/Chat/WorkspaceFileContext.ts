import { createContext, useContext } from 'react';

/**
 * Where a piece of markdown "lives" in the workspace, so embedded content
 * that references sibling files by relative path (a `.vl.json` chart spec,
 * the CSV a spec points at through `data.url`) can be resolved and read
 * through the workspace file API.
 *
 * - `workspaceId`: the workspace whose files relative paths resolve into.
 * - `basePath`: workspace-relative path of the document being rendered; the
 *   directory part is the base for relative references. Empty string means
 *   the workspace root (chat messages have no file of their own).
 * - `onOpenFile`: opens a workspace-relative file in its drawer; an embedded
 *   chart offers it as "Open". Omit where opening has nowhere to go (a
 *   read-only transcript, the file drawer itself).
 *
 * `null` (no provider) means relative references cannot be resolved; the
 * consumer should degrade to an explanatory placeholder, not throw.
 */
export interface WorkspaceFileLocation {
  workspaceId: string;
  basePath: string;
  onOpenFile?: (path: string) => void;
}

export const WorkspaceFileContext = createContext<WorkspaceFileLocation | null>(null);

export const useWorkspaceFileLocation = (): WorkspaceFileLocation | null =>
  useContext(WorkspaceFileContext);
