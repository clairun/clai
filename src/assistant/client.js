/**
 * Assistant Engine Client
 *
 * Thin wrapper around Tauri invoke calls for the assistant engine.
 */

import { invoke } from '@tauri-apps/api/core';

export async function createSession(params) {
  return invoke('assistant_create_session', { request: params });
}

export async function getSession(sessionId) {
  return invoke('assistant_get_session', { sessionId });
}

export async function listSessions(tabId = null) {
  return invoke('assistant_list_sessions', { tabId });
}

export async function deleteSession(sessionId) {
  return invoke('assistant_delete_session', { sessionId });
}

export async function loadSessionMessages(sessionId) {
  return invoke('assistant_load_session_messages', { sessionId });
}

export async function sendMessage(sessionId, message, connectionId) {
  return invoke('assistant_send_message', { sessionId, message, connectionId });
}

export async function listRuns(sessionId) {
  return invoke('assistant_list_runs', { sessionId });
}

export async function listToolCalls(sessionId, runId = null) {
  return invoke('assistant_list_tool_calls', {
    request: {
      sessionId,
      runId,
    },
  });
}

export async function retryRun(runId, connectionId) {
  return invoke('assistant_retry_run', { runId, connectionId });
}

export async function cancelRun(runId) {
  return invoke('assistant_cancel_run', { runId });
}

export async function listProviderConnections() {
  return invoke('provider_connection_list');
}

export async function getProviderConnection(id) {
  return invoke('provider_connection_get', { id });
}

export async function createProviderConnection(request) {
  return invoke('provider_connection_create', { request });
}

export async function updateProviderConnection(request) {
  return invoke('provider_connection_update', { request });
}

export async function deleteProviderConnection(id) {
  return invoke('provider_connection_delete', { id });
}

export async function listProviderModels(id) {
  return invoke('provider_connection_list_models', { id });
}

export async function listProviderDescriptorModels(providerId) {
  return invoke('provider_descriptor_models', { providerId });
}

export async function testProviderConnection(id) {
  return invoke('provider_connection_test', { id });
}

export async function listAvailableProviderAdapters() {
  return invoke('provider_connection_list_available');
}
