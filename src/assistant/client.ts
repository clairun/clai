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
  AssistantMessage,
  AssistantRun,
  AssistantSession,
  CreateProviderConnectionRequest,
  ModelInfo,
  ProviderConnection,
  ProviderDescriptor,
  TestResult,
  ToolInvocation,
  UpdateProviderConnectionRequest,
} from '../generated/bindings';

export async function createSession(params: unknown): Promise<AssistantSession> {
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

export async function sendMessage(
  sessionId: string,
  message: string,
  connectionId: string,
): Promise<{ session: AssistantSession; message: AssistantMessage; run?: AssistantRun | null; queued: boolean }> {
  return invoke('assistant_send_message', { sessionId, message, connectionId });
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
