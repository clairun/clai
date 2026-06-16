/**
 * Window-level UI command events.
 *
 * The floating terminal (MainLayout) and the workspace chrome
 * (FleetLayout) live in separate React subtrees, so slash commands that
 * trigger workspace UI (settings modal, fork) are delivered as window
 * CustomEvents — the same decoupling the MCP/provider "changed" events
 * already use.
 */

export const WORKSPACE_UI_COMMAND_EVENT = 'clai-workspace-ui-command';
const PENDING_FORK_PROMPTS_KEY = 'clai.pendingForkPrompts';

export type WorkspaceUiAction = 'settings' | 'fork';

export interface WorkspaceUiCommandDetail {
  action: WorkspaceUiAction;
  workspaceId: string;
  prompt?: string | null;
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

const readPendingForkPrompts = (): Record<string, string> => {
  try {
    const raw = sessionStorage.getItem(PENDING_FORK_PROMPTS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object') return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(([, value]) => typeof value === 'string')
    ) as Record<string, string>;
  } catch {
    return {};
  }
};

const writePendingForkPrompts = (prompts: Record<string, string>): void => {
  try {
    sessionStorage.setItem(PENDING_FORK_PROMPTS_KEY, JSON.stringify(prompts));
  } catch {
    /* ignore storage failure */
  }
};

export const setPendingForkPrompt = (workspaceId: string, prompt: string): void => {
  const trimmed = prompt.trim();
  if (!workspaceId || !trimmed) return;
  const prompts = readPendingForkPrompts();
  prompts[workspaceId] = trimmed;
  writePendingForkPrompts(prompts);
};

export const takePendingForkPrompt = (workspaceId: string): string | null => {
  if (!workspaceId) return null;
  const prompts = readPendingForkPrompts();
  const prompt = prompts[workspaceId] || null;
  if (prompt !== null) {
    delete prompts[workspaceId];
    writePendingForkPrompts(prompts);
  }
  return prompt;
};

/**
 * Fire-and-forget signal asking the active chat conversation to scroll to the
 * bottom (if the reader was already near the bottom). Used when entering
 * terminal mode shrinks the conversation viewport, so the latest messages stay
 * in view instead of being scrolled off the top.
 */
export const SCROLL_CHAT_TO_BOTTOM_EVENT = 'clai-scroll-chat-bottom';

export const dispatchScrollChatToBottom = (): void => {
  window.dispatchEvent(new Event(SCROLL_CHAT_TO_BOTTOM_EVENT));
};

export const onScrollChatToBottom = (handler: () => void): (() => void) => {
  const listener = () => handler();
  window.addEventListener(SCROLL_CHAT_TO_BOTTOM_EVENT, listener);
  return () => window.removeEventListener(SCROLL_CHAT_TO_BOTTOM_EVENT, listener);
};
