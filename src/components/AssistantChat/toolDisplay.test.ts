import { describe, expect, it } from 'vitest';
import {
  asPayloadObject,
  cleanToolName,
  guessLang,
  summarizeToolCall,
  summarizeToolResult,
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
});

describe('summarizeToolResult', () => {
  it('shows bash exit code with tone', () => {
    expect(summarizeToolResult('bash_exec', { exitCode: 0 }, null, 'completed')).toEqual({
      text: 'exit 0',
      tone: 'neutral',
    });
    expect(summarizeToolResult('bash_exec', { exitCode: 1 }, null, 'completed')).toEqual({
      text: 'exit 1',
      tone: 'error',
    });
  });

  it('counts lines / entries / results', () => {
    expect(summarizeToolResult('fs_read', { content: 'a\nb\nc' }, null, 'completed')).toEqual({
      text: '3 lines',
      tone: 'neutral',
    });
    expect(summarizeToolResult('fs_list', { entries: [1, 2] }, null, 'completed')).toEqual({
      text: '2 entries',
      tone: 'neutral',
    });
    expect(summarizeToolResult('web_search', { results: [1] }, null, 'completed')).toEqual({
      text: '1 result',
      tone: 'neutral',
    });
  });

  it('returns null while running and for unknown tools', () => {
    expect(summarizeToolResult('fs_read', null, null, 'running')).toBeNull();
    expect(summarizeToolResult('some_mcp_tool', { ok: true }, null, 'completed')).toBeNull();
  });

  it('flags errors', () => {
    expect(summarizeToolResult('web_fetch', null, 'boom', 'failed')).toEqual({
      text: 'error',
      tone: 'error',
    });
  });

  // A CLI provider (Claude Code) runs our built-ins over MCP and reports the
  // wire envelope, so the payload arrives as a JSON string inside a content
  // part. Without unwrapping, every one of these summaries reads an object
  // with none of its keys: "0 entries" for a real listing, no exit code at all.
  it('summarizes results wrapped in an MCP content envelope', () => {
    const wrap = (payload: unknown) => [{ type: 'text', text: JSON.stringify(payload) }];

    expect(summarizeToolResult('bash_exec', wrap({ exitCode: 2 }), null, 'completed')).toEqual({
      text: 'exit 2',
      tone: 'error',
    });
    expect(summarizeToolResult('fs_list', wrap({ entries: [1, 2, 3] }), null, 'completed')).toEqual(
      { text: '3 entries', tone: 'neutral' }
    );
    expect(summarizeToolResult('fs_read', wrap({ content: 'a\nb' }), null, 'completed')).toEqual({
      text: '2 lines',
      tone: 'neutral',
    });
    expect(summarizeToolResult('fs_glob', wrap({ matches: [1] }), null, 'completed')).toEqual({
      text: '1 match',
      tone: 'neutral',
    });
  });

  it('summarizes results wrapped in an MCP client envelope object', () => {
    const result = {
      serverId: 'srv-1',
      toolName: 'fs_list',
      content: [{ type: 'text', text: '{"entries":[1,2]}' }],
      text: '{"entries":[1,2]}',
    };
    expect(summarizeToolResult('fs_list', result, null, 'completed')).toEqual({
      text: '2 entries',
      tone: 'neutral',
    });
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
