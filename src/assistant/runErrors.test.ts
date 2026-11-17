import { describe, expect, it } from 'vitest';
import type { AssistantRun, FailureKind } from '../generated/bindings';
import { runFailure } from './runErrors';

const run = (status: AssistantRun['status'], error: string | null, errorKind?: FailureKind): AssistantRun => ({
  id: 'r1', sessionId: 's', status, trigger: 'user_message', connectionId: 'c',
  protocolId: 'p', modelId: 'm', startedAt: 1n, completedAt: 2n, error, errorKind,
});

describe('runFailure', () => {
  it.each<[FailureKind, string | undefined]>([
    ['auth', 'Update the key in Settings → AI Provider.'],
    ['usage_limit', 'Wait for it to reset, or raise the limit with the provider.'],
    ['model_unavailable', 'Pick another model.'],
    ['content_filtered', 'Rephrase the request and try again.'],
    // The error text already names the remedy for these.
    ['billing', undefined],
    ['transient', undefined],
    ['context_too_long', undefined],
    ['other', undefined],
  ])('gives %s failures the action %j', (kind, action) => {
    expect(runFailure(run('failed', 'HTTP 400: boom', kind))).toEqual({ text: 'HTTP 400: boom', kind, action });
  });

  it('leaves a failure without a kind unmarked, whatever its text says', () => {
    const failure = runFailure(run('failed', "You've hit your usage limit · resets 4:40pm"));
    expect(failure).toEqual({ text: "You've hit your usage limit · resets 4:40pm" });
    expect(failure?.kind).toBeUndefined();
    expect(failure?.action).toBeUndefined();
  });

  it('falls back to a generic text when the error is empty', () => {
    expect(runFailure(run('failed', '  '))?.text).toBe('The run failed.');
  });

  it('reports nothing for a run that did not fail', () => {
    expect(runFailure(run('completed_with_warnings', 'One tool failed', 'other'))).toBeNull();
    expect(runFailure(run('completed', null))).toBeNull();
    expect(runFailure(null)).toBeNull();
  });
});
