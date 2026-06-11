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
import { getOrCreateWorkspaceSession } from '../../workspace/client';
import TerminalEmulator from './TerminalEmulator';

const errorMessage = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const OPEN_WORKSPACE_ERROR = 'Open a workspace to chat with the assistant.';

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
    async (query: string): Promise<{ error?: string }> => {
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

        const result = await assistantClient.sendMessage(binding.session.id, query, connectionId);
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
      agentWorking={inputDisabled}
    />
  );
};

export default TerminalEmulatorWrapper;
