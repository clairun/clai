/**
 * TerminalEmulatorWrapper
 *
 * Routes free-text terminal prompts into the assistant engine. The target
 * (see `useTerminalTarget`) is the open workspace's manager session, or on
 * the Fleet the selected card's — keyed `workspace:<id>` in the assistant
 * store. On the Fleet with no card selected, a prompt creates a workspace
 * (with the model and MCP chosen in the terminal) and sends to it, images
 * included.
 */

import { useCallback, useMemo } from 'react';
import { useAssistantStore, assistantClient } from '../../assistant';
import { readImage } from '@tauri-apps/plugin-clipboard-manager';
import { mainRunning } from '../../fleet/fleetModel';
import { useFleetStore } from '../../fleet/fleetStore';
import { useFleetActivityStore } from '../../stores/fleetActivityStore';
import { useTaskStore } from '../../workspace/taskStore';
import {
  applyNewWorkspaceDraft,
  draftConnectionId,
  draftImagePart,
  dropDraftImages,
  storeDraftImages,
} from '../../fleet/newWorkspaceDraft';
import {
  createWorkspace,
  getOrCreateWorkspaceSession,
  storeWorkspaceImage,
  pickAndStoreWorkspaceImage,
  pickImage,
} from '../../workspace/client';
import type { ContentPart } from '../../generated/bindings';
import { openGlobalSettings } from '../../utils/globalSettings';
import TerminalEmulator from './TerminalEmulator';
import { useTerminalTarget } from './useTerminalTarget';

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

const NO_CONNECTION_ERROR =
  'Add an enabled assistant provider connection before sending prompts from this workspace.';

const openProviderSetup = () => openGlobalSettings({ tab: 'provider', providerAction: 'new' });

/** Send `query` to `workspaceId`'s main session, binding it in the assistant store. */
export async function sendToWorkspace(
  workspaceId: string,
  query: string,
  images: ContentPart[]
): Promise<{ error?: string }> {
  const binding = await getOrCreateWorkspaceSession(workspaceId);
  const connectionId = binding.providerConnectionId;
  if (!connectionId) {
    openProviderSetup();
    return { error: NO_CONNECTION_ERROR };
  }

  const store = useAssistantStore.getState();
  store.initSession(binding.session);
  // Bridge workspace session to the chat panel via a synthetic tab key
  store.setActiveSessionForTab(`workspace:${workspaceId}`, binding.session.id);

  const result = await assistantClient.sendMessage(
    workspaceId,
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
  useFleetStore.getState().noteUserSent(workspaceId, binding.session.id, result.message);
  return {};
}

/** Why images can't go to `workspaceId` (`null`: the Fleet's new workspace), or `null` if they can. */
async function imageAttachError(workspaceId: string | null): Promise<string | null> {
  const connectionId = workspaceId
    ? (await getOrCreateWorkspaceSession(workspaceId)).providerConnectionId
    : draftConnectionId(await assistantClient.listProviderConnections());
  if (!connectionId) return 'Add an enabled assistant provider connection before attaching images.';
  const supported = await assistantClient.connectionSupportsImages(connectionId);
  return supported ? null : 'The selected model does not support image input.';
}

/** A workspace being created from the Fleet; prompts sent meanwhile join it instead of creating another. */
let creatingWorkspace: Promise<string> | null = null;

/** Checks for a provider first, so a first run without one leaves no empty workspace behind. */
async function createFleetWorkspace(): Promise<string> {
  const connections = await assistantClient.listProviderConnections();
  if (!connections.some((c) => c.enabled)) {
    openProviderSetup();
    throw NO_CONNECTION_ERROR;
  }
  const workspaceId = await createWorkspace();
  await applyNewWorkspaceDraft(workspaceId, connections);
  return workspaceId;
}

/** Create a workspace and send to it; select it once the send worked, unless the user picked a card meanwhile. */
export async function sendToNewWorkspace(query: string, images: ContentPart[]): Promise<{ error?: string }> {
  const creating = (creatingWorkspace ??= createFleetWorkspace());
  try {
    const workspaceId = await creating;
    const result = await sendToWorkspace(workspaceId, query, await storeDraftImages(workspaceId, images));
    if (!result.error) dropDraftImages(images);
    const fleet = useFleetStore.getState();
    if (!result.error && !fleet.selectedId) fleet.select(workspaceId);
    return result;
  } finally {
    if (creatingWorkspace === creating) creatingWorkspace = null;
  }
}

const TerminalEmulatorWrapper = () => {
  const target = useTerminalTarget();
  const targetWorkspaceId = target.workspaceId;
  const onFleet = target.surface === 'fleet';

  // Track whether the target's main session has a non-terminal run so the
  // input can show queued-message wording while still accepting text. The
  // workspace page populates the `workspace:<id>` tab-key on load; the Fleet
  // doesn't load its cards' sessions, so there its live runs say it.
  const workspaceSessionId = useAssistantStore(
    (state) =>
      (targetWorkspaceId && state.activeSessionByTab[`workspace:${targetWorkspaceId}`]) || null
  );
  const workspaceIsStreaming = useAssistantStore(
    (state) => (workspaceSessionId ? !!state.sessions[workspaceSessionId]?.isStreaming : false)
  );

  // When a run fails before producing anything (429/400/token-limit), the BE
  // retracts the user message and the store stashes its text here so the
  // composer can restore the lost prompt instead of forcing a retype.
  const recoverablePrompt = useAssistantStore(
    (state) => (workspaceSessionId ? state.recoverablePrompts[workspaceSessionId] ?? '' : '')
  );
  const handleRecoverablePromptConsumed = useCallback(() => {
    if (workspaceSessionId) {
      useAssistantStore.getState().clearRecoverablePrompt(workspaceSessionId);
    }
  }, [workspaceSessionId]);

  const fleetCard = useFleetStore((s) => (onFleet && targetWorkspaceId ? s.fleet.cards[targetWorkspaceId] : undefined));
  const fleetRuns = useFleetActivityStore((s) => (fleetCard ? s.activeRunsByWorkspace[fleetCard.workspaceId] : undefined));
  const fleetTasks = useTaskStore((s) => (fleetCard ? s.tasks[fleetCard.workspaceId] : undefined));
  const fleetMainWorking = useMemo(
    () =>
      !!fleetCard &&
      mainRunning(fleetCard, {
        runs: { [fleetCard.workspaceId]: fleetRuns ?? [] },
        tasks: { [fleetCard.workspaceId]: fleetTasks ?? {} },
      }),
    [fleetCard, fleetRuns, fleetTasks]
  );

  const inputDisabled = !!targetWorkspaceId && (workspaceIsStreaming || fleetMainWorking);

  /**
   * Handle sending a query through the assistant engine.
   */
  const handleSendToAgent = useCallback(
    async (query: string, images: ContentPart[] = []): Promise<{ error?: string }> => {
      try {
        // On the Fleet the selection is read now, not at render: a card
        // selected since (or by a create that just finished) is the target.
        const workspaceId = onFleet ? useFleetStore.getState().selectedId : targetWorkspaceId;
        if (workspaceId) return await sendToWorkspace(workspaceId, query, images);
        if (onFleet) return await sendToNewWorkspace(query, images);
        return { error: OPEN_WORKSPACE_ERROR };
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Workspace assistant error:', err);
        return {
          error: errorMessage(err, 'Assistant request failed.'),
        };
      }
    },
    [onFleet, targetWorkspaceId]
  );

  /**
   * Resolve + store a pasted/attached image, gated on the target connection's
   * image capability. Returns the `ContentPart` to attach, or an error string
   * to surface in the composer. The gate is the same backend resolver the
   * send-filter uses, so the UI never offers what the provider would drop. On
   * the Fleet with no card selected the image waits in the new-workspace draft.
   */
  const handleAttachImage = useCallback(
    async (file: File): Promise<{ part?: ContentPart; error?: string }> => {
      if (!targetWorkspaceId && !onFleet) return { error: OPEN_WORKSPACE_ERROR };
      try {
        const error = await imageAttachError(targetWorkspaceId);
        if (error) return { error };
        const dataBase64 = await fileToBase64(file);
        const filename = file.name || null;
        const part = targetWorkspaceId
          ? await storeWorkspaceImage(targetWorkspaceId, dataBase64, file.type, filename)
          : draftImagePart({ dataBase64, mediaType: file.type, filename });
        return { part };
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Workspace image attach error:', err);
        return { error: errorMessage(err, 'Could not attach image.') };
      }
    },
    [targetWorkspaceId, onFleet]
  );

  /**
   * Reliable cross-platform attach: open the native file dialog, gate on the
   * target connection, and store the chosen image. Clipboard paste is flaky in
   * the Linux WebKitGTK webview, so this picker is the primary affordance.
   */
  const handlePickImage = useCallback(async (): Promise<{
    part?: ContentPart;
    error?: string;
  }> => {
    if (!targetWorkspaceId && !onFleet) return { error: OPEN_WORKSPACE_ERROR };
    try {
      const error = await imageAttachError(targetWorkspaceId);
      if (error) return { error };
      // Dialog runs backend-side: the picked path never crosses the
      // renderer boundary, closing the arbitrary-file-read hole.
      const part = targetWorkspaceId
        ? await pickAndStoreWorkspaceImage(targetWorkspaceId)
        : await pickImage().then((image) => image && draftImagePart(image));
      return part ? { part } : {}; // null: user cancelled, no attachment, no error
    } catch (err) {
      console.error('[TerminalEmulatorWrapper] Workspace image pick error:', err);
      return { error: errorMessage(err, 'Could not attach image.') };
    }
  }, [targetWorkspaceId, onFleet]);

  const handleAgentCommand = useCallback(
    async (command: string): Promise<{ error?: string; message?: string }> => {
      const [name] = command.trim().split(/\s+/, 1);

      const currentWorkspaceId = targetWorkspaceId;
      if (!currentWorkspaceId) {
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
          await assistantClient.deleteSession(currentWorkspaceId, binding.session.id);
          store.removeSession(binding.session.id);
          const fresh = await getOrCreateWorkspaceSession(currentWorkspaceId);
          store.initSession(fresh.session);
          store.setActiveSessionForTab(`workspace:${currentWorkspaceId}`, fresh.session.id);
          useFleetStore.getState().noteUserSent(currentWorkspaceId, fresh.session.id, null);
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
        const result = await assistantClient.compactSession(currentWorkspaceId, binding.session.id, connectionId);
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
    [targetWorkspaceId]
  );

  return (
    <TerminalEmulator
      onSendToChat={handleSendToAgent}
      onAgentCommand={handleAgentCommand}
      onAttachImage={handleAttachImage}
      onPickImage={handlePickImage}
      onReadClipboardImage={readClipboardImageAsFile}
      agentWorking={inputDisabled}
      recoverablePrompt={recoverablePrompt}
      onRecoverablePromptConsumed={handleRecoverablePromptConsumed}
    />
  );
};

export default TerminalEmulatorWrapper;
