/**
 * Assistant Engine Client
 *
 * Thin wrapper around Tauri invoke calls for the assistant engine.
 * Request/response shapes that come from the bindings are typed;
 * provider-connection commands still use loose types until their
 * payload structs get `#[derive(TS)]`.
 */

import { invoke } from '@tauri-apps/api/core';
import type {
  AssistantCompaction,
  AssistantMessage,
  ContentPart,
  AssistantMessageCursor,
  AssistantMessagePage,
  AssistantRun,
  AssistantSession,
  CreateProviderConnectionRequest,
  ModelInfo,
  ProviderConnection,
  ProbeModelsRequest,
  ProviderCatalogEntry,
  ProviderDescriptor,
  PendingAsk,
  TestResult,
  UpdateProviderConnectionRequest,
} from '../generated/bindings';

export async function getSession(workspaceId: string, sessionId: string): Promise<AssistantSession | null> {
  return invoke('assistant_get_session', { workspaceId, sessionId });
}

/** Questions `ask_user` is waiting on in this workspace, so a reloaded view can show them again. */
export async function listPendingAsks(workspaceId: string): Promise<PendingAsk[]> {
  return invoke('assistant_list_pending_asks', { workspaceId });
}

export async function deleteSession(workspaceId: string, sessionId: string): Promise<void> {
  return invoke('assistant_delete_session', { workspaceId, sessionId });
}

export async function loadSessionMessagesPage(params: {
  workspaceId: string;
  sessionId: string;
  before?: AssistantMessageCursor | null;
  limit?: number;
}): Promise<AssistantMessagePage> {
  return invoke('assistant_load_session_messages_page', {
    request: {
      workspaceId: params.workspaceId,
      sessionId: params.sessionId,
      before: params.before ?? null,
      limit: params.limit ?? null,
    },
  });
}

export async function sendMessage(
  workspaceId: string,
  sessionId: string,
  message: string,
  connectionId: string,
  images: ContentPart[] = [],
): Promise<{ session: AssistantSession; message: AssistantMessage; run?: AssistantRun | null; queued: boolean }> {
  return invoke('assistant_send_message', { workspaceId, sessionId, message, connectionId, images });
}

/**
 * Whether the connection's active model accepts image input. Backed by the same
 * resolver as the send-filter, so the composer's paste affordance can't drift
 * from what the backend will actually send.
 */
export async function connectionSupportsImages(connectionId: string): Promise<boolean> {
  return invoke('assistant_connection_supports_images', { connectionId });
}

export async function compactSession(
  workspaceId: string,
  sessionId: string,
  connectionId: string,
): Promise<{ compaction?: AssistantCompaction | null; summaryMessage?: AssistantMessage | null }> {
  return invoke('assistant_compact_session', { workspaceId, sessionId, connectionId });
}

/**
 * Delete a user message that is still waiting in the queue (not yet picked
 * up by a run). Errors if a run grabbed it in the meantime. The backend
 * emits MessageDeleted, which removes it from the store.
 */
export async function deleteQueuedMessage(workspaceId: string, sessionId: string, messageId: string): Promise<void> {
  return invoke('assistant_delete_queued_message', { workspaceId, sessionId, messageId });
}

/**
 * Edit the text of a queued user message in-place (still waiting, not yet
 * picked up by a run). Errors if a run grabbed it in the meantime. The
 * backend emits AssistantMessageUpdated, which swaps the text in the store.
 */
export async function editQueuedMessage(
  workspaceId: string,
  sessionId: string,
  messageId: string,
  text: string,
): Promise<void> {
  return invoke('assistant_edit_queued_message', { workspaceId, sessionId, messageId, text });
}

/** Only the newest run plus the active run (if older), newest first — not the run history. */
export async function listRuns(workspaceId: string, sessionId: string): Promise<AssistantRun[]> {
  return invoke('assistant_list_runs', { workspaceId, sessionId });
}

/**
 * Full result of one tool call. Live events and message pages carry only a
 * compact view of it, so the expanded row fetches the rest on demand. The
 * lookup is keyed on the tool call's own `sessionId`, not the session being
 * viewed.
 */
export async function getToolCallResult(workspaceId: string, sessionId: string, toolCallId: string): Promise<unknown> {
  return invoke('assistant_get_tool_call_result', { workspaceId, sessionId, toolCallId });
}

/**
 * Full input of one tool call; events and message pages carry a projection of
 * it. Keyed on the tool call's own `sessionId`, as for `getToolCallResult`.
 */
export async function getToolCallInput(workspaceId: string, sessionId: string, toolCallId: string): Promise<unknown> {
  return invoke('assistant_get_tool_call_input', { workspaceId, sessionId, toolCallId });
}

export async function cancelRun(workspaceId: string, runId: string): Promise<AssistantRun> {
  return invoke('assistant_cancel_run', { workspaceId, runId });
}

// ── Provider-connection commands ────────────────────────────────────
// Typed end-to-end via the generated bindings. Request shapes
// (CreateProviderConnectionRequest, UpdateProviderConnectionRequest)
// and response shapes (ProviderConnection, ProviderDescriptor,
// ModelInfo, TestResult) all come from #[derive(TS)] on the Rust side.

export async function listProviderConnections(): Promise<ProviderConnection[]> {
  return invoke('provider_connection_list');
}

/** Last-4 hint for ONE connection's stored API key. Looked up lazily when
 * the user opens a connection for editing — a keyring read per call, so
 * never fanned out across the whole list. */
export async function getProviderSecretHint(id: string): Promise<string | null> {
  return invoke('provider_connection_secret_hint', { id });
}

export async function createProviderConnection(
  request: CreateProviderConnectionRequest,
): Promise<ProviderConnection> {
  return invoke('provider_connection_create', { request });
}

export async function updateProviderConnection(
  request: UpdateProviderConnectionRequest,
): Promise<ProviderConnection> {
  return invoke('provider_connection_update', { request });
}

export async function deleteProviderConnection(id: string): Promise<void> {
  return invoke('provider_connection_delete', { id });
}

export async function listProviderDescriptorModels(providerId: string): Promise<ModelInfo[]> {
  return invoke('provider_descriptor_models', { providerId });
}

export async function testProviderConnection(id: string): Promise<TestResult> {
  return invoke('provider_connection_test', { id });
}

export async function listAvailableProviderAdapters(): Promise<ProviderDescriptor[]> {
  return invoke('provider_connection_list_available');
}

export async function listProviderCatalog(): Promise<ProviderCatalogEntry[]> {
  return invoke('provider_catalog_list');
}

export async function probeCatalogModels(request: ProbeModelsRequest): Promise<ModelInfo[]> {
  return invoke('provider_catalog_probe_models', { request });
}
