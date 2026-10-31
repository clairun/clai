import { createContext, useContext, useMemo } from 'react';
import { resolveWorkspacePath } from '../../utils/htmlBundle';

/**
 * Where a piece of markdown "lives" in the workspace, so embedded content
 * that references sibling files by relative path (a `.vl.json` chart spec,
 * the CSV a spec points at through `data.url`, an image, a link to another
 * file) can be resolved and read through the workspace file API.
 *
 * - `workspaceId`: the workspace whose files relative paths resolve into.
 * - `basePath`: workspace-relative path of the document being rendered; the
 *   directory part is the base for relative references. Empty string means
 *   the workspace root (chat messages have no file of their own).
 * - `rootPath`: absolute filesystem path of the workspace, when known, so a
 *   reference written as an absolute path inside it resolves too.
 * - `openFile`: opens a workspace file in the app's preview panel. Without
 *   it, links to workspace files don't navigate.
 *
 * `null` (no provider) means relative references cannot be resolved; the
 * consumer should degrade to an explanatory placeholder, not throw.
 */
export interface WorkspaceFileLocation {
  workspaceId: string;
  basePath: string;
  rootPath?: string;
  openFile?: (path: string, line?: number) => void;
}

export const WorkspaceFileContext = createContext<WorkspaceFileLocation | null>(null);

export const useWorkspaceFileLocation = (): WorkspaceFileLocation | null =>
  useContext(WorkspaceFileContext);

/**
 * The location of a document in `workspaceId`, keeping the enclosing
 * provider's workspace-wide `rootPath` and `openFile` when it is for the same
 * workspace. `openFile` overrides the inherited opener.
 */
export const useDocumentLocation = (
  workspaceId: string,
  basePath: string,
  openFile?: (path: string, line?: number) => void,
): WorkspaceFileLocation => {
  const parent = useWorkspaceFileLocation();
  const inherited = parent?.workspaceId === workspaceId ? parent : null;
  const rootPath = inherited?.rootPath;
  const opener = openFile ?? inherited?.openFile;
  return useMemo(
    () => ({ workspaceId, basePath, rootPath, openFile: opener }),
    [workspaceId, basePath, rootPath, opener],
  );
};

const decodePath = (href: string): string => {
  try {
    return decodeURI(href);
  } catch {
    return href; // a stray `%` that is part of the file name
  }
};

/**
 * Resolve a workspace reference from markdown (an image `src` or link `href`
 * without a URL scheme, see `isWorkspaceRelativeHref`) to a workspace-relative
 * path:
 * - relative: against the document (chat: the workspace root);
 * - leading `/`: the workspace root;
 * - an absolute filesystem path inside `rootPath`: that file.
 * Markdown percent-encodes targets, so they are decoded; `?query`/`#fragment`
 * are dropped. Returns '' for the workspace root itself.
 */
export const resolveWorkspaceHref = (
  location: Pick<WorkspaceFileLocation, 'basePath' | 'rootPath'> | null,
  href: string,
): string => {
  const target = decodePath(href.trim().replace(/[?#].*$/, ''));
  const root = location?.rootPath?.replace(/\/+$/, '');
  if (root && (target === root || target.startsWith(`${root}/`))) {
    return resolveWorkspacePath('', target.slice(root.length));
  }
  return resolveWorkspacePath(location?.basePath ?? '', target);
};

/** A literal trailing `:line` is link metadata; `%3A` keeps a filename colon. */
export const resolveWorkspaceFileLink = (
  location: Pick<WorkspaceFileLocation, 'basePath' | 'rootPath'> | null,
  href: string,
): { path: string; line?: number } => {
  const target = href.trim().replace(/[?#].*$/, '');
  const match = /^(.*):([1-9]\d*)$/.exec(target);
  const line = match ? Number(match[2]) : undefined;
  // decodeURI preserves encoded reserved characters, including a filename colon.
  if (match?.[1] && Number.isSafeInteger(line)) {
    return { path: resolveWorkspaceHref(location, match[1].replace(/%3a/gi, ':')), line };
  }
  return { path: resolveWorkspaceHref(location, href.replace(/%3a/gi, ':')) };
};
