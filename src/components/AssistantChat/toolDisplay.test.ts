import { describe, expect, it } from 'vitest';
import {
  asParamsObject,
  asPayloadObject,
  cleanToolName,
  inlineTaskCard,
  guessLang,
  chartArtifactPath,
  summarizeToolCall,
  toPreviewText,
} from './toolDisplay';

describe('summarizeToolCall', () => {
  it('maps built-in tools to a verb + primary arg', () => {
    expect(summarizeToolCall('fs_read', { path: '/a/b.ts' })).toEqual({ verb: 'Read', arg: '/a/b.ts' });
    expect(summarizeToolCall('bash_exec', { command: 'npm run build' })).toEqual({
      verb: 'Bash',
      arg: 'npm run build',
    });
    expect(summarizeToolCall('web_search', { query: 'rust async' })).toEqual({
      verb: 'Search',
      arg: 'rust async',
    });
    expect(summarizeToolCall('fs_glob', { pattern: '**/*.rs' })).toEqual({ verb: 'Glob', arg: '**/*.rs' });
    expect(summarizeToolCall('ask_user', { question: 'How should I proceed?' })).toEqual({
      verb: 'Ask',
      arg: 'How should I proceed?',
    });
  });

  it('collapses multi-line commands onto one line', () => {
    expect(summarizeToolCall('bash_exec', { command: 'echo a\n  echo b' }).arg).toBe('echo a echo b');
  });

  it('strips the mcp prefix and shows the first scalar param for unknown tools', () => {
    const s = summarizeToolCall('mcp.abc123.get_metric_data', { context: 'system.cpu', after: 5 });
    expect(s.verb).toBe('get_metric_data');
    expect(s.arg).toBe('system.cpu');
  });

  it('parses JSON-string params', () => {
    expect(summarizeToolCall('fs_read', '{"path":"/x.ts"}')).toEqual({ verb: 'Read', arg: '/x.ts' });
  });

  it('reads params that carry a `text` string or a `content` array as the input, not as an envelope', () => {
    expect(summarizeToolCall('mcp__slack__post', { text: 'do it', channel: 'x' })).toEqual({
      verb: 'post',
      arg: 'do it',
    });
    expect(
      summarizeToolCall('mcp__x__create', { content: [{ type: 'text', text: 'hi' }], target: 'y' })
    ).toEqual({ verb: 'create', arg: 'y' });
  });
});

describe('chart calls', () => {
  it('are labelled by title', () => {
    expect(summarizeToolCall('create_vega_chart', { title: 'Q3 Revenue', spec: {} })).toEqual({
      verb: 'Chart',
      arg: 'Q3 Revenue',
    });
  });
});

describe('chartArtifactPath', () => {
  const result = { ok: true, path: 'charts/q3.vl.json' };

  it('returns the saved path of a completed chart call', () => {
    expect(chartArtifactPath('create_vega_chart', result, null, 'completed')).toBe('charts/q3.vl.json');
    // Old persisted results may still carry `display`; it no longer matters.
    expect(
      chartArtifactPath('create_vega_chart', { ...result, display: false }, null, 'completed')
    ).toBe('charts/q3.vl.json');
    // Claude Code stores the JSON as text; Codex reaches us via MCP envelopes.
    expect(
      chartArtifactPath('mcp__clai__create_vega_chart', JSON.stringify(result), null, 'completed')
    ).toBe('charts/q3.vl.json');
    expect(
      chartArtifactPath(
        'create_vega_chart',
        { content: [{ type: 'text', text: JSON.stringify(result) }] },
        null,
        'completed'
      )
    ).toBe('charts/q3.vl.json');
  });

  it('returns null when there is no saved chart', () => {
    expect(chartArtifactPath('create_vega_chart', result, 'schema error', 'failed')).toBeNull();
    expect(chartArtifactPath('create_vega_chart', null, null, 'running')).toBeNull();
    expect(chartArtifactPath('create_vega_chart', { ok: false }, null, 'completed')).toBeNull();
    expect(chartArtifactPath('create_vega_chart', null, null, 'completed')).toBeNull();
    expect(chartArtifactPath('fs_write', result, null, 'completed')).toBeNull();
  });
});

describe('asParamsObject', () => {
  it('passes plain objects and JSON strings through', () => {
    expect(asParamsObject({ path: '/a' })).toEqual({ path: '/a' });
    expect(asParamsObject('{"path":"/a"}')).toEqual({ path: '/a' });
  });

  it('never unwraps: a `text` string or `content` array is the tool input', () => {
    const withText = { text: 'do it', channel: 'x' };
    expect(asParamsObject(withText)).toBe(withText);
    const withContent = { content: [{ type: 'text', text: 'hi' }], target: 'y' };
    expect(asParamsObject(withContent)).toBe(withContent);
  });

  it('yields null for arrays, non-objects and non-object JSON', () => {
    expect(asParamsObject([{ type: 'text', text: '{"a":1}' }])).toBeNull();
    expect(asParamsObject(null)).toBeNull();
    expect(asParamsObject(42)).toBeNull();
    expect(asParamsObject('"just a string"')).toBeNull();
  });
});

describe('asPayloadObject', () => {
  it('passes plain objects and JSON strings through', () => {
    expect(asPayloadObject({ a: 1 })).toEqual({ a: 1 });
    expect(asPayloadObject('{"a":1}')).toEqual({ a: 1 });
  });

  it('unwraps MCP content envelopes carrying a JSON object', () => {
    expect(asPayloadObject([{ type: 'text', text: '{"a":1}' }])).toEqual({ a: 1 });
    expect(asPayloadObject({ content: [{ type: 'text', text: '{"a":1}' }] })).toEqual({ a: 1 });
  });

  it('yields nothing when the envelope text is not a JSON object', () => {
    // An MCP tool answering in prose (or with a JSON array) has no payload
    // object; the envelope's own keys are the wire, not the tool's answer, so
    // the per-tool formatters must not see them.
    expect(
      asPayloadObject({ content: [{ type: 'text', text: 'all good' }], text: 'all good' })
    ).toBeNull();
    expect(asPayloadObject([{ type: 'text', text: '[1,2]' }])).toBeNull();
  });

  it('does not mistake a string `content` field for an envelope', () => {
    // fs_read results carry the file in `content`; promoting it would replace
    // the result with whatever the file happens to contain.
    expect(asPayloadObject({ path: '/a.json', content: '{"a":1}' })).toEqual({
      path: '/a.json',
      content: '{"a":1}',
    });
  });

  it('promotes nothing from a multi-part envelope, in either shape', () => {
    // rmcp's `structured()` emits exactly one text part for our built-ins, so a
    // multi-part envelope is a third-party MCP result: the envelope IS the
    // payload and there is no tool JSON to promote. The joined text is not a
    // JSON object, and both envelope shapes must agree on that.
    const parts = [
      { type: 'text', text: '{"a":1}' },
      { type: 'text', text: 'trailing prose' },
    ];
    expect(asPayloadObject(parts)).toBeNull();
    expect(asPayloadObject({ serverId: 's', toolName: 't', content: parts })).toBeNull();
  });

  it('unwraps a client envelope that carries only `text`', () => {
    expect(asPayloadObject({ serverId: 's', toolName: 't', text: '{"entries":[1,2]}' })).toEqual({
      entries: [1, 2],
    });
  });

  it('ignores content parts that are not text parts, in either shape', () => {
    // Only `type: 'text'` parts carry the payload; a resource part's own text
    // would otherwise be joined in front of it and break the parse.
    const parts = [
      { type: 'resource', text: 'file:///a.txt' },
      { type: 'text', text: '{"a":1}' },
    ];
    expect(asPayloadObject({ content: parts })).toEqual({ a: 1 });
    expect(asPayloadObject(parts)).toEqual({ a: 1 });
  });

  it('yields null for non-objects', () => {
    expect(asPayloadObject(null)).toBeNull();
    expect(asPayloadObject(42)).toBeNull();
    expect(asPayloadObject('not json')).toBeNull();
  });
});

describe('toPreviewText', () => {
  it('joins bash stdout and stderr', () => {
    expect(toPreviewText('bash_exec', { stdout: 'out', stderr: 'err' }, null)).toBe('out\nerr');
    expect(toPreviewText('bash_exec', { stdout: '', stderr: '' }, null)).toBe('(no output)');
  });

  it('returns file content verbatim', () => {
    expect(toPreviewText('fs_read', { content: 'line1\nline2' }, null)).toBe('line1\nline2');
  });

  it('extracts MCP envelope text', () => {
    expect(toPreviewText('mcp.x.y', { content: [{ type: 'text', text: 'hello' }] }, null)).toBe('hello');
  });

  it('formats built-in results wrapped in an MCP content envelope', () => {
    const wrap = (payload: unknown) => [{ type: 'text', text: JSON.stringify(payload) }];

    expect(toPreviewText('bash_exec', wrap({ stdout: 'out', stderr: 'err' }), null)).toBe(
      'out\nerr'
    );
    expect(toPreviewText('fs_read', wrap({ content: 'line1\nline2' }), null)).toBe(
      'line1\nline2'
    );
    expect(
      toPreviewText('fs_list', wrap({ entries: [{ path: '/a' }, { path: '/b' }] }), null)
    ).toBe('/a\n/b');
  });

  it('joins the text parts of a multi-part envelope, in either shape', () => {
    const parts = [
      { type: 'text', text: 'first' },
      { type: 'text', text: 'second' },
    ];
    expect(toPreviewText('mcp.x.y', parts, null)).toBe('first\n\nsecond');
    expect(toPreviewText('mcp.x.y', { content: parts }, null)).toBe('first\n\nsecond');
  });

  it('prefers the error message', () => {
    expect(toPreviewText('bash_exec', { stdout: 'out' }, 'failed to spawn')).toBe('failed to spawn');
  });

  it('pretty-prints unknown JSON objects', () => {
    expect(toPreviewText('weird', { a: 1 }, null)).toBe('{\n  "a": 1\n}');
  });
});

describe('guessLang', () => {
  it('maps extensions', () => {
    expect(guessLang('src/a.tsx')).toBe('tsx');
    expect(guessLang('main.rs')).toBe('rust');
    expect(guessLang('script.sh')).toBe('bash');
    expect(guessLang('notes.md')).toBe('markdown');
  });
  it('returns empty for unknown / missing', () => {
    expect(guessLang('file.xyz')).toBe('');
    expect(guessLang(undefined)).toBe('');
    expect(guessLang('Makefile')).toBe('');
  });
});

describe('cleanToolName', () => {
  it('strips the dotted mcp prefix', () => {
    expect(cleanToolName('mcp.uuid-123.get_data')).toBe('get_data');
    expect(cleanToolName('bash_exec')).toBe('bash_exec');
  });
  it('strips the Claude Code mcp__server__ prefix', () => {
    expect(cleanToolName('mcp__clai__bash_exec')).toBe('bash_exec');
    expect(cleanToolName('mcp__net_data__get_metric_data')).toBe('get_metric_data');
  });
});

describe('summarizeToolCall via mcp__ prefix', () => {
  it('maps an mcp-bridged bash_exec to the Bash verb + command', () => {
    expect(summarizeToolCall('mcp__clai__bash_exec', { command: 'go test ./...' })).toEqual({
      verb: 'Bash',
      arg: 'go test ./...',
    });
  });
});

describe('inlineTaskCard', () => {
  const task = (over: Record<string, unknown> = {}) => ({
    ok: true,
    task: {
      id: 'task-1',
      workspaceId: 'ws-1',
      assignedToWorkspaceAgentId: 'wa-7',
      assignedAgentDefinitionId: 'def-7',
      title: 'Round 3 review',
      instructions: '  Review the branch end to end.  ',
      status: 'queued',
      resultSummary: null,
      error: null,
      ...over,
    },
  });

  it('reads an assignment into a full card', () => {
    expect(inlineTaskCard('workspace_assignTask', task(), null, 'completed')).toEqual({
      kind: 'assign',
      variant: 'full',
      taskId: 'task-1',
      title: 'Round 3 review',
      instructions: 'Review the branch end to end.',
      status: 'queued',
      assignedToWorkspaceAgentId: 'wa-7',
      assignedAgentDefinitionId: 'def-7',
      detail: '',
      detailIsError: false,
    });
  });

  it('keeps a poll of an unfinished task slim, queued as well as running', () => {
    // A worker that has not been picked up yet answers `queued`, and a wait is
    // mostly these: they collapse with the run instead of each drawing a card.
    for (const status of ['running', 'queued']) {
      expect(
        inlineTaskCard('workspace_getTaskResult', task({ status }), null, 'completed')
      ).toMatchObject({ kind: 'poll', variant: 'slim', status });
    }
  });

  it('clamps what a card renders, so a whole brief is not a whole DOM node', () => {
    const long = 'x'.repeat(400);
    const card = inlineTaskCard(
      'workspace_getTaskResult',
      task({ status: 'completed', instructions: long, resultSummary: long, title: long }),
      null,
      'completed'
    );
    expect(card?.instructions).toHaveLength(301);
    expect(card?.instructions.endsWith('…')).toBe(true);
    expect(card?.detail).toHaveLength(301);
    expect(card?.title).toHaveLength(301);
    // Anything that already fits is left exactly as it came.
    const short = inlineTaskCard('workspace_assignTask', task(), null, 'completed');
    expect(short?.instructions).toBe('Review the branch end to end.');
  });

  it('gives a hand-off a full card whatever status it was stamped with', () => {
    // TaskCard leans on this: a delegation never collapses into the slim run,
    // so the full card is the only shape a hand-off ever takes.
    for (const status of ['queued', 'running', 'completed', 'failed']) {
      expect(inlineTaskCard('workspace_assignTask', task({ status }), null, 'completed'))
        .toMatchObject({ kind: 'assign', variant: 'full', status });
    }
  });

  it('gives every terminal status a full card', () => {
    for (const status of ['completed', 'failed', 'blocked']) {
      expect(inlineTaskCard('workspace_getTaskResult', task({ status }), null, 'completed'))
        .toMatchObject({ variant: 'full', status });
    }
  });

  it('prefers the task error over its summary, and says which it is', () => {
    const card = inlineTaskCard(
      'workspace_getTaskResult',
      task({ status: 'failed', resultSummary: 'partial work', error: '  boom  ' }),
      null,
      'completed'
    );
    expect(card).toMatchObject({ detail: 'boom', detailIsError: true });
  });

  it('shows the summary when the task carries no error', () => {
    const card = inlineTaskCard(
      'workspace_getTaskResult',
      task({ status: 'completed', resultSummary: 'Found 3 issues.' }),
      null,
      'completed'
    );
    expect(card).toMatchObject({ detail: 'Found 3 issues.', detailIsError: false });
  });

  it('unwraps the MCP envelope the bridged tools answer in', () => {
    const envelope = { content: [{ type: 'text', text: JSON.stringify(task()) }] };
    expect(inlineTaskCard('mcp__clai__workspace_assignTask', envelope, null, 'completed'))
      .toMatchObject({ taskId: 'task-1', kind: 'assign' });
  });

  it('leaves the plain row in place for anything it cannot draw', () => {
    // Another tool.
    expect(inlineTaskCard('bash_exec', task(), null, 'completed')).toBeNull();
    // The call itself failed or is still going — no task to show.
    expect(inlineTaskCard('workspace_assignTask', task(), 'bad agent id', 'failed')).toBeNull();
    expect(inlineTaskCard('workspace_assignTask', undefined, null, 'running')).toBeNull();
    // Payloads that are not a task.
    expect(inlineTaskCard('workspace_assignTask', { ok: false }, null, 'completed')).toBeNull();
    expect(inlineTaskCard('workspace_assignTask', { ok: true }, null, 'completed')).toBeNull();
    expect(
      inlineTaskCard('workspace_assignTask', task({ id: '' }), null, 'completed')
    ).toBeNull();
    expect(
      inlineTaskCard('workspace_assignTask', task({ title: '' }), null, 'completed')
    ).toBeNull();
    expect(
      inlineTaskCard('workspace_assignTask', task({ status: '' }), null, 'completed')
    ).toBeNull();
    expect(
      inlineTaskCard(
        'workspace_assignTask',
        task({ assignedToWorkspaceAgentId: '' }),
        null,
        'completed'
      )
    ).toBeNull();
  });
});
