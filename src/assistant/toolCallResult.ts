/**
 * Lazy access to a tool call's full input and result. The chat only receives
 * a compact view of each call, so an expanded row fetches the rest through
 * here.
 */

import { useEffect, useState } from 'react';
import type { ToolInvocation } from '../generated/bindings';
import { getToolCallInput, getToolCallResult } from './client';

type ToolCallRef = Pick<ToolInvocation, 'id' | 'sessionId' | 'status' | 'completedAt'>;
type Detail = 'input' | 'result';

export type ToolCallDetailState =
  | { state: 'idle' }
  | { state: 'loading' }
  | { state: 'ready'; value: unknown }
  | { state: 'error'; message: string; retryable: boolean };

// Full inputs and results can be whole files, so only the most recently
// viewed stay resident; an evicted entry costs a refetch.
const CACHE_LIMIT = 30;
const cache = new Map<string, unknown>();
const inflight = new Map<string, Promise<unknown>>();

// A result's key includes status and completion, so one fetched while the
// call was still running is never shown as its final output. Input never
// changes after the call starts.
const detailKey = (detail: Detail, call: ToolCallRef): string =>
  detail === 'input'
    ? `input:${call.sessionId}:${call.id}`
    : `result:${call.sessionId}:${call.id}:${call.status}:${call.completedAt ?? ''}`;

const remember = (key: string, value: unknown) => {
  cache.delete(key);
  cache.set(key, value);
  while (cache.size > CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
};

const fetchDetail = (detail: Detail, call: ToolCallRef): Promise<unknown> => {
  const key = detailKey(detail, call);
  if (cache.has(key)) {
    const value = cache.get(key);
    remember(key, value);
    return Promise.resolve(value);
  }
  let pending = inflight.get(key);
  if (!pending) {
    const load = detail === 'input' ? getToolCallInput : getToolCallResult;
    pending = load(call.sessionId, call.id)
      .then((value) => {
        const result = value ?? null;
        remember(key, result);
        return result;
      })
      .finally(() => inflight.delete(key));
    inflight.set(key, pending);
  }
  return pending;
};

/** Test-only: forget every cached and in-flight detail. */
export const clearToolCallResultCache = () => {
  cache.clear();
  inflight.clear();
};

const MESSAGES: Record<Detail, { gone: string; failed: string }> = {
  input: { gone: 'Input no longer available', failed: "Couldn't load input" },
  result: { gone: 'Output no longer available', failed: "Couldn't load output" },
};

const toErrorState = (detail: Detail, err: unknown): ToolCallDetailState => {
  const text = err instanceof Error ? err.message : String(err ?? '');
  return /not found/i.test(text)
    ? { state: 'error', message: MESSAGES[detail].gone, retryable: false }
    : { state: 'error', message: MESSAGES[detail].failed, retryable: true };
};

const useToolCallDetail = (
  detail: Detail,
  call: ToolCallRef | undefined,
  enabled: boolean,
): ToolCallDetailState & { retry: () => void } => {
  const key = call && enabled ? detailKey(detail, call) : null;
  const sessionId = call?.sessionId;
  const id = call?.id;
  const status = call?.status;
  const completedAt = call?.completedAt;
  const [settled, setSettled] = useState<{ key: string; value: ToolCallDetailState } | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    // A cache hit still resolves into `settled`, so the row keeps its value if
    // the entry is evicted while it stays open.
    if (!key || sessionId === undefined || id === undefined || status === undefined) return;
    let cancelled = false;
    fetchDetail(detail, { sessionId, id, status, completedAt: completedAt ?? null }).then(
      (value) => {
        if (!cancelled) setSettled({ key, value: { state: 'ready', value } });
      },
      (err: unknown) => {
        if (!cancelled) setSettled({ key, value: toErrorState(detail, err) });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [detail, key, sessionId, id, status, completedAt, attempt]);

  const retry = () => {
    setSettled(null);
    setAttempt((n) => n + 1);
  };

  if (!key) return { state: 'idle', retry };
  if (cache.has(key)) return { state: 'ready', value: cache.get(key), retry };
  if (settled?.key === key) return { ...settled.value, retry };
  return { state: 'loading', retry };
};

/** The full result of `call` while `enabled`. Errors are not cached: `retry` fetches again. */
export const useToolCallResult = (call: ToolCallRef | undefined, enabled: boolean) =>
  useToolCallDetail('result', call, enabled);

/** The full input of `call` while `enabled`. Errors are not cached: `retry` fetches again. */
export const useToolCallInput = (call: ToolCallRef | undefined, enabled: boolean) =>
  useToolCallDetail('input', call, enabled);
