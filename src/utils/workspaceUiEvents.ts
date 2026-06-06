/**
 * Window-level UI command events.
 *
 * The floating terminal (MainLayout) and the workspace chrome
 * (FleetLayout) live in separate React subtrees, so slash commands that
 * trigger workspace UI (settings modal, clone) are delivered as window
 * CustomEvents — the same decoupling the MCP/provider "changed" events
 * already use.
 */

export const WORKSPACE_UI_COMMAND_EVENT = 'clai-workspace-ui-command';

export type WorkspaceUiAction = 'settings' | 'clone';

export interface WorkspaceUiCommandDetail {
  action: WorkspaceUiAction;
  workspaceId: string;
}

export const dispatchWorkspaceUiCommand = (detail: WorkspaceUiCommandDetail): void => {
  window.dispatchEvent(new CustomEvent(WORKSPACE_UI_COMMAND_EVENT, { detail }));
};

export const onWorkspaceUiCommand = (
  handler: (detail: WorkspaceUiCommandDetail) => void
): (() => void) => {
  const listener = (event: Event) => {
    const detail = (event as CustomEvent<WorkspaceUiCommandDetail>).detail;
    if (!detail?.action || !detail.workspaceId) return;
    handler(detail);
  };
  window.addEventListener(WORKSPACE_UI_COMMAND_EVENT, listener);
  return () => window.removeEventListener(WORKSPACE_UI_COMMAND_EVENT, listener);
};
