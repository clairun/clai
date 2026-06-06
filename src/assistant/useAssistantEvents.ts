/**
 * Assistant Event Reducer
 *
 * Subscribes to Tauri 'assistant://event' events and dispatches updates
 * to the assistant session store. The envelope shape comes from
 * `src/generated/bindings.ts` — every variant of `AssistantUiEvent` is
 * type-checked at the switch below, so adding/renaming a Rust variant
 * surfaces as a compile error here (after regenerating bindings).
 */

import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import useAssistantStore from './sessionStore';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import type { AssistantEventEnvelope, AssistantUiEvent } from '../generated/bindings';

const ASSISTANT_EVENT_NAME = 'assistant://event';

interface TauriEvent<T> {
  payload: T;
}

const handleEvent = (envelope: AssistantEventEnvelope): void => {
  const { sessionId } = envelope;
  const event: AssistantUiEvent = envelope.event;
  const store = useAssistantStore.getState();

  switch (event.type) {
    case 'session_created':
      store.initSession(event.payload.session);
      break;

    case 'message_created':
      store.addMessage(sessionId, event.payload.message);
      break;

    case 'message_deleted':
      // A user message whose run failed before the provider produced
      // anything (or its empty assistant placeholder), or a queued
      // message the user removed before pickup — drop it so it doesn't
      // linger unanswered.
      store.removeMessage(sessionId, event.payload.message_id);
      break;

    case 'queued_messages_delivered':
      // The queued messages were handed to a run — clear their chips.
      store.markQueuedMessagesDelivered(sessionId, event.payload.message_ids);
      break;

    case 'session_compacted':
      store.addMessage(sessionId, event.payload.summary_message);
      break;

    case 'run_queued':
    case 'run_started':
    case 'run_completed':
    case 'run_failed':
    case 'run_cancelled': {
      store.setRunStatus(sessionId, event.payload.run);
      // Mirror run lifecycle into the global fleet-activity store so Fleet
      // cards/counter reflect in-flight runs and survive navigation. Keyed
      // by the envelope's workspace id (set for workspace/agent sessions).
      const workspaceId = envelope.workspaceId;
      if (workspaceId) {
        const fleet = useFleetActivityStore.getState();
        if (event.type === 'run_queued' || event.type === 'run_started') {
          fleet.markRunStarted(workspaceId, event.payload.run.id);
        } else {
          fleet.markRunEnded(workspaceId, event.payload.run.id);
        }
      }
      break;
    }

    case 'assistant_delta':
      store.appendDelta(sessionId, event.payload.message_id, event.payload.text);
      break;

    case 'assistant_thinking_delta':
      // Thinking deltas are intentionally not surfaced in the chat
      // store today; they exist on the BE for provider compatibility.
      break;

    case 'assistant_message_completed':
      store.completeMessage(sessionId, event.payload.message);
      break;

    case 'assistant_message_updated':
      // Mid-turn content flush from local_agent's persist_tool_use —
      // refresh message.content so the chat picks up the new tool_use
      // parts without waiting for assistant_message_completed.
      store.updateMessageContent(sessionId, event.payload.message);
      break;

    case 'tool_call_started':
    case 'tool_call_completed':
    case 'tool_call_failed':
      store.setToolCall(sessionId, event.payload.tool_call);
      break;

    case 'ask_user_requested':
      // The `ask_user` tool is blocking the run waiting for input.
      store.setAskUserPending(sessionId, {
        pendingId: event.payload.pending_id,
        question: event.payload.question,
        options: event.payload.options,
        multiSelect: event.payload.multi_select,
        extraContext: event.payload.extra_context ?? null,
      });
      break;

    case 'ask_user_resolved':
      store.clearAskUserPending(sessionId, event.payload.pending_id);
      break;

    default: {
      // Exhaustiveness: the bindings union must cover every emitted
      // variant. If a new Rust variant lands without an arm here, this
      // line will fail to type-check.
      const _exhaustive: never = event;
      void _exhaustive;
    }
  }
};

export function useAssistantEvents(): void {
  useEffect(() => {
    const unlistenPromise = listen<AssistantEventEnvelope>(
      ASSISTANT_EVENT_NAME,
      (tauriEvent: TauriEvent<AssistantEventEnvelope>) => {
        const envelope = tauriEvent.payload;
        if (!envelope || !envelope.event) return;
        handleEvent(envelope);
      },
    );

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);
}
