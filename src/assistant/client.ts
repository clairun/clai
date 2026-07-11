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
  TestResult,
  ToolInvocation,
  UpdateProviderConnectionRequest,
} from '../generated/bindings';

export async function createSession(params: {
  tabId?: string;
  kind?: 'interactive' | 'background_job';
  title?: string;
  context?: Record<string, unknown>;
  parentSessionId?: string | null;
}): Promise<AssistantSession> {
  return invoke('assistant_create_session', { request: params });
}

export async function getSession(sessionId: string): Promise<AssistantSession | null> {
  return invoke('assistant_get_session', { sessionId });
}

export async function listSessions(tabId: string | null = null): Promise<AssistantSession[]> {
  return invoke('assistant_list_sessions', { tabId });
}

export async function deleteSession(sessionId: string): Promise<void> {
  return invoke('assistant_delete_session', { sessionId });
}

export async function loadSessionMessages(sessionId: string): Promise<AssistantMessage[]> {
  return invoke('assistant_load_session_messages', { sessionId });
}

export async function loadSessionMessagesPage(params: {
  sessionId: string;
  before?: AssistantMessageCursor | null;
  limit?: number;
  includeAncestors?: boolean;
}): Promise<AssistantMessagePage> {
  return invoke('assistant_load_session_messages_page', {
    request: {
      sessionId: params.sessionId,
      before: params.before ?? null,
      limit: params.limit ?? null,
      includeAncestors: params.includeAncestors ?? true,
    },
  });
}

export async function sendMessage(
  sessionId: string,
  message: string,
  connectionId: string,
  images: ContentPart[] = [],
): Promise<{ session: AssistantSession; message: AssistantMessage; run?: AssistantRun | null; queued: boolean }> {
  return invoke('assistant_send_message', { sessionId, message, connectionId, images });
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
  sessionId: string,
  connectionId: string,
): Promise<{ compaction?: AssistantCompaction | null; summaryMessage?: AssistantMessage | null }> {
  return invoke('assistant_compact_session', { sessionId, connectionId });
}

/**
 * Delete a user message that is still waiting in the queue (not yet picked
 * up by a run). Errors if a run grabbed it in the meantime. The backend
 * emits MessageDeleted, which removes it from the store.
 */
export async function deleteQueuedMessage(sessionId: string, messageId: string): Promise<void> {
  return invoke('assistant_delete_queued_message', { sessionId, messageId });
}

/**
 * Edit the text of a queued user message in-place (still waiting, not yet
 * picked up by a run). Errors if a run grabbed it in the meantime. The
 * backend emits AssistantMessageUpdated, which swaps the text in the store.
 */
export async function editQueuedMessage(
  sessionId: string,
  messageId: string,
  text: string,
): Promise<void> {
  return invoke('assistant_edit_queued_message', { sessionId, messageId, text });
}

export async function listRuns(sessionId: string): Promise<AssistantRun[]> {
  return invoke('assistant_list_runs', { sessionId });
}

export async function listToolCalls(
  sessionId: string,
  runId: string | null = null,
): Promise<ToolInvocation[]> {
  return invoke('assistant_list_tool_calls', {
    request: {
      sessionId,
      runId,
    },
  });
}

export async function retryRun(runId: string, connectionId: string): Promise<AssistantRun> {
  return invoke('assistant_retry_run', { runId, connectionId });
}

export async function cancelRun(runId: string): Promise<AssistantRun> {
  return invoke('assistant_cancel_run', { runId });
}

// ── Provider-connection commands ────────────────────────────────────
// Typed end-to-end via the generated bindings. Request shapes
// (CreateProviderConnectionRequest, UpdateProviderConnectionRequest)
// and response shapes (ProviderConnection, ProviderDescriptor,
// ModelInfo, TestResult) all come from #[derive(TS)] on the Rust side.

export async function listProviderConnections(): Promise<ProviderConnection[]> {
  return invoke('provider_connection_list');
}

/** Last-4 hints for stored provider API keys, keyed by connection id.
 * Separate from the list call: each hint costs a keyring read, so only the
 * Settings surface should ask for them. */
export async function listProviderSecretHints(): Promise<Record<string, string>> {
  return invoke('provider_connection_secret_hints');
}

export async function getProviderConnection(id: string): Promise<ProviderConnection | null> {
  return invoke('provider_connection_get', { id });
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

export async function listProviderModels(id: string): Promise<ModelInfo[]> {
  return invoke('provider_connection_list_models', { id });
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
