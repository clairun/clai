/**
 * TerminalEmulatorWrapper
 *
 * Routes free-text terminal prompts into the assistant engine. Prompts only
 * have a target on workspace routes (the workspace's manager session, keyed
 * `workspace:<id>` in the assistant store); on other routes the terminal
 * accepts slash commands but tells the user to open a workspace to chat.
 */

import { useCallback } from 'react';
import { useLocation } from 'react-router-dom';
import { useAssistantStore, assistantClient } from '../../assistant';
import { readImage } from '@tauri-apps/plugin-clipboard-manager';
import {
  getOrCreateWorkspaceSession,
  storeWorkspaceImage,
  pickAndStoreWorkspaceImage,
} from '../../workspace/client';
import type { ContentPart } from '../../generated/bindings';
import TerminalEmulator from './TerminalEmulator';

const errorMessage = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const OPEN_WORKSPACE_ERROR = 'Open a workspace to chat with the assistant.';

/** Read a File into raw base64 (no `data:` URL prefix). */
const fileToBase64 = (file: File): Promise<string> =>
  new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error('Failed to read file'));
    reader.onload = () => {
      const result = typeof reader.result === 'string' ? reader.result : '';
      const comma = result.indexOf(',');
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.readAsDataURL(file);
  });

/**
 * Read an image from the native OS clipboard and convert it to a PNG `File`.
 * Returns `null` when the clipboard holds no image, so an ordinary text paste
 * can proceed. WebKit webviews (Linux/mac, what Claude Code users run) don't
 * surface pasted images to the DOM paste event, so this native read is the
 * only reliable Ctrl/Cmd+V image path there.
 */
async function readClipboardImageAsFile(): Promise<File | null> {
  let image;
  try {
    image = await readImage();
  } catch (err) {
    // A plain text paste lands here too (no image on the clipboard), so this is
    // not necessarily an error — but a genuine clipboard failure (e.g. the
    // Flatpak X11 socket being withheld so arboard's XWayland fallback can't
    // reach an X server) is otherwise indistinguishable and silent. Log at
    // debug so the real cause is recoverable from devtools without noise.
    console.debug('readImage() returned no image (text paste or clipboard error):', err);
    return null;
  }
  try {
    const rgba = await image.rgba();
    const { width, height } = await image.size();
    if (!width || !height) return null;
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) return null;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
    const blob: Blob | null = await new Promise((resolve) =>
      canvas.toBlob(resolve, 'image/png')
    );
    if (!blob) return null;
    return new File([blob], 'pasted.png', { type: 'image/png' });
  } finally {
    try {
      await image.close();
    } catch {
      // best-effort release of the native image resource
    }
  }
}

const TerminalEmulatorWrapper = () => {
  const location = useLocation();
  const workspaceRouteMatch = location.pathname.match(/^\/workspace(?:\/([^/]+))?\/?$/);
  const isWorkspaceRoute = Boolean(workspaceRouteMatch);
  const currentWorkspaceId = workspaceRouteMatch?.[1]
    ? decodeURIComponent(workspaceRouteMatch[1])
    : 'default';

  // Track whether the workspace's canonical manager session has a
  // non-terminal run so the input can show queued-message wording while
  // still accepting text. Workspace.tsx populates the `workspace:<id>`
  // tab-key on load.
  const workspaceSessionId = useAssistantStore(
    (state) => state.activeSessionByTab[`workspace:${currentWorkspaceId}`] || null
  );
  const workspaceIsStreaming = useAssistantStore(
    (state) => (workspaceSessionId ? !!state.sessions[workspaceSessionId]?.isStreaming : false)
  );

  const inputDisabled = isWorkspaceRoute && workspaceIsStreaming;

  /**
   * Handle sending a query through the assistant engine.
   */
  const handleSendToAgent = useCallback(
    async (query: string, images: ContentPart[] = []): Promise<{ error?: string }> => {
      if (!isWorkspaceRoute) {
        return { error: OPEN_WORKSPACE_ERROR };
      }

      try {
        const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
        const connectionId = binding.providerConnectionId;
        if (!connectionId) {
          return {
            error: 'Add an enabled assistant provider connection before sending prompts from this workspace.',
          };
        }

        const store = useAssistantStore.getState();
        store.initSession(binding.session);

        // Bridge workspace session to the chat panel via a synthetic tab key
        const workspaceTabKey = `workspace:${currentWorkspaceId}`;
        store.setActiveSessionForTab(workspaceTabKey, binding.session.id);

        const result = await assistantClient.sendMessage(
          binding.session.id,
          query,
          connectionId,
          images,
        );
        store.addMessage(binding.session.id, result.message);
        if (result.queued) {
          // Sent while a run was active — show the "Queued" chip until a
          // run picks it up (queued_messages_delivered clears it).
          store.markMessageQueued(binding.session.id, result.message.id);
        }
        return {};
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Workspace assistant error:', err);
        return {
          error: errorMessage(err, 'Assistant request failed.'),
        };
      }
    },
    [currentWorkspaceId, isWorkspaceRoute]
  );

  /**
   * Resolve + store a pasted/attached image, gated on the active connection's
   * image capability. Returns the stored `ContentPart` to attach, or an error
   * string to surface in the composer. The gate is the same backend resolver
   * the send-filter uses, so the UI never offers what the provider would drop.
   */
  const handleAttachImage = useCallback(
    async (file: File): Promise<{ part?: ContentPart; error?: string }> => {
      if (!isWorkspaceRoute) {
        return { error: OPEN_WORKSPACE_ERROR };
      }
      try {
        const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
        const connectionId = binding.providerConnectionId;
        if (!connectionId) {
          return {
            error: 'Add an enabled assistant provider connection before attaching images.',
          };
        }
        const supported = await assistantClient.connectionSupportsImages(connectionId);
        if (!supported) {
          return { error: 'The selected model does not support image input.' };
        }
        const dataBase64 = await fileToBase64(file);
        const part = await storeWorkspaceImage(
          currentWorkspaceId,
          dataBase64,
          file.type,
          file.name || null,
        );
        return { part };
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Workspace image attach error:', err);
        return { error: errorMessage(err, 'Could not attach image.') };
      }
    },
    [currentWorkspaceId, isWorkspaceRoute]
  );

  /**
   * Reliable cross-platform attach: open the native file dialog, gate on the
   * active connection, and store the chosen image. Clipboard paste is flaky in
   * the Linux WebKitGTK webview, so this picker is the primary affordance.
   */
  const handlePickImage = useCallback(async (): Promise<{
    part?: ContentPart;
    error?: string;
  }> => {
    if (!isWorkspaceRoute) {
      return { error: OPEN_WORKSPACE_ERROR };
    }
    try {
      const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
      const connectionId = binding.providerConnectionId;
      if (!connectionId) {
        return {
          error: 'Add an enabled assistant provider connection before attaching images.',
        };
      }
      const supported = await assistantClient.connectionSupportsImages(connectionId);
      if (!supported) {
        return { error: 'The selected model does not support image input.' };
      }
      // Dialog runs backend-side: the picked path never crosses the
      // renderer boundary, closing the arbitrary-file-read hole.
      const part = await pickAndStoreWorkspaceImage(currentWorkspaceId);
      if (!part) {
        return {}; // user cancelled, no attachment, no error
      }
      return { part };
    } catch (err) {
      console.error('[TerminalEmulatorWrapper] Workspace image pick error:', err);
      return { error: errorMessage(err, 'Could not attach image.') };
    }
  }, [currentWorkspaceId, isWorkspaceRoute]);

  const handleAgentCommand = useCallback(
    async (command: string): Promise<{ error?: string; message?: string }> => {
      const [name] = command.trim().split(/\s+/, 1);

      if (!isWorkspaceRoute) {
        return { error: OPEN_WORKSPACE_ERROR };
      }

      // /clear — HARD clear: deletes the session (DB cascades messages,
      // runs, tool calls, compaction summaries; artifacts/memories/tasks
      // are workspace-scoped and survive). The backend refuses while a
      // run is active. A fresh empty session is bound immediately.
      if (name === 'clear') {
        try {
          const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
          const store = useAssistantStore.getState();
          await assistantClient.deleteSession(binding.session.id);
          store.removeSession(binding.session.id);
          const fresh = await getOrCreateWorkspaceSession(currentWorkspaceId);
          store.initSession(fresh.session);
          store.setActiveSessionForTab(`workspace:${currentWorkspaceId}`, fresh.session.id);
          return { message: 'Conversation history cleared.' };
        } catch (err) {
          console.error('[TerminalEmulatorWrapper] Workspace clear error:', err);
          return { error: errorMessage(err, 'Clear failed.') };
        }
      }

      if (name !== 'compact') {
        return { error: `Unknown assistant command: /${name || command}` };
      }

      try {
        const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
        const connectionId = binding.providerConnectionId;
        if (!connectionId) {
          return {
            error: 'Add an enabled assistant provider connection before compacting this workspace session.',
          };
        }
        const store = useAssistantStore.getState();
        store.initSession(binding.session);
        store.setActiveSessionForTab(`workspace:${currentWorkspaceId}`, binding.session.id);
        const result = await assistantClient.compactSession(binding.session.id, connectionId);
        if (result.summaryMessage) {
          store.addMessage(binding.session.id, result.summaryMessage);
        }
        return {
          message: result.compaction
            ? result.compaction.strategy === 'session_rotation_summary'
              ? 'Conversation compacted. The CLI session will rotate on the next prompt.'
              : 'Conversation compacted.'
            : 'There is not enough history to compact yet.',
        };
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Workspace compaction error:', err);
        return {
          error: errorMessage(err, 'Compaction failed.'),
        };
      }
    },
    [currentWorkspaceId, isWorkspaceRoute]
  );

  return (
    <TerminalEmulator
      onSendToChat={handleSendToAgent}
      onAgentCommand={handleAgentCommand}
      onAttachImage={handleAttachImage}
      onPickImage={handlePickImage}
      onReadClipboardImage={readClipboardImageAsFile}
      agentWorking={inputDisabled}
    />
  );
};

export default TerminalEmulatorWrapper;
