import { describe, expect, it } from 'vitest';
import {
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
