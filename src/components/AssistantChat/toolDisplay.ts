/**
 * toolDisplay — pure helpers that turn a tool call's name/params/result into
 * the compact, human-readable pieces the chat renders:
 *   - summarizeToolCall:   the one-line row label   → { verb, arg }
 *   - summarizeToolResult: the right-aligned hint    → { text, tone } | null
 *   - toPreviewText:       plain text for the inline preview / terminal view
 *   - guessLang:           a fence language from a file path
 *
 * Kept free of React so it can be unit-tested directly and reused by both the
 * workspace chat and the fleet detail pane. Everything is best-effort and
 * never throws on malformed data — the chat must render whatever the provider
 * sends.
 */

/** Result/summary tone — drives colour on the row summary. */
export type ResultTone = 'neutral' | 'error';

export interface ToolCallSummary {
  /** Human verb, e.g. "Read", "Bash", or a cleaned tool name for MCP tools. */
  verb: string;
  /** Primary argument, single-line (path, command, query, …). May be empty. */
  arg: string;
}

export interface ToolResultSummary {
  text: string;
  tone: ResultTone;
}

/**
 * Clean MCP-style tool names down to the bare tool name. Tools reach us under
 * two prefixed conventions depending on the transport:
 *   - dotted (local routing):   "mcp.<uuid>.get_metric_data"
 *   - Claude Code / CLI bridge: "mcp__<server>__bash_exec"
 * Both must collapse to the bare name so the per-tool formatting (Bash, Read, …)
 * applies regardless of whether a tool ran via the local agent or over MCP.
 */
export const cleanToolName = (name: string): string => {
  if (!name) return name;
  // "mcp__<server>__<tool>" — server names use single underscores, tool names
  // don't contain "__", so the last "__"-delimited segment is the tool.
  if (name.startsWith('mcp__')) {
    const parts = name.split('__').filter(Boolean);
    if (parts.length >= 2) return parts[parts.length - 1]!;
  }
  // "mcp.<uuid-or-id>.<tool>"
  const match = name.match(/^mcp\.[^.]+\.(.+)$/);
  return match ? match[1]! : name;
};

interface McpTextPart {
  type?: string;
  text?: string;
}

/**
 * Join the `type: 'text'` parts of an MCP content array. Other part kinds
 * (images, embedded resources — which carry text of their own) are not the
 * tool's answer and would corrupt the join. Null when there is no text part.
 */
const joinTextParts = (parts: unknown[]): string | null => {
  const texts = (parts as McpTextPart[])
    .filter((p) => p && p.type === 'text' && typeof p.text === 'string')
    .map((p) => p.text as string);
  return texts.length > 0 ? texts.join('\n\n') : null;
};

/**
 * Extract displayable text from an MCP-style result. MCP results can be an
 * envelope { content: [{type:"text", text}], text, … }, a bare content array,
 * a plain string, or a generic object. Returns null when no text is found.
 */
export const extractMcpText = (result: unknown): string | null => {
  if (typeof result === 'string') return result;
  if (!result || typeof result !== 'object') return null;

  const envelope = result as { content?: unknown; text?: unknown };

  if (Array.isArray(envelope.content)) {
    const joined = joinTextParts(envelope.content);
    if (joined !== null) return joined;
  }

  if (typeof envelope.text === 'string' && envelope.text.trim()) {
    return envelope.text;
  }

  if (Array.isArray(result)) {
    const joined = joinTextParts(result);
    if (joined !== null) return joined;
  }

  return null;
};

/** Parse a JSON object out of text; null for anything that isn't one. */
const parseJsonObject = (text: string): Record<string, unknown> | null => {
  const trimmed = text.trim();
  if (!trimmed.startsWith('{') || !trimmed.endsWith('}')) return null;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
};

/**
 * Coerce a tool's params into a plain object: the object itself, or a
 * JSON-encoded string of one. Params never travel in an MCP envelope, so a
 * params object that happens to carry a `text` string or a `content` array
 * (both common in third-party tool schemas) is the tool's input, not a wrapper
 * — it must not go through `asPayloadObject`, which would unwrap it to nothing.
 */
export const asParamsObject = (value: unknown): Record<string, unknown> | null => {
  if (typeof value === 'string') return parseJsonObject(value);
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  return null;
};

/**
 * Coerce a tool's result into the plain object the per-tool formatters read.
 * (Params take `asParamsObject`: they never travel in an envelope.)
 *
 * A result reaches the chat in one of three shapes, and all three describe the
 * same tool output:
 *   - the object itself           — the Anthropic provider stores the tool's JSON value;
 *   - a JSON-encoded string       — Claude Code stringifies it;
 *   - an MCP content envelope     — Codex reaches our built-ins through the local
 *     MCP server and stores the wire form,
 *     `[{ type: 'text', text: '<the JSON>' }]`, as does our own MCP client with
 *     `{ content: [...], text }`.
 * The envelope must be unwrapped here or every per-tool formatter reads an
 * object that has none of the keys it looks for: a 40-file listing summarised
 * as "0 entries", a bash run with no exit code, a file preview showing escaped
 * JSON instead of the file.
 *
 * An envelope describes the wire, not the tool, so it yields the JSON object it
 * carries or nothing at all: falling back to its own keys would hand the
 * formatters `{ serverId, toolName, content, text }`. An MCP tool answering in
 * prose therefore has no payload object — which costs nothing, since the
 * per-tool formatters only run for our built-ins and the prose still reaches
 * the preview through `extractMcpText`.
 */
export const asPayloadObject = (value: unknown): Record<string, unknown> | null => {
  if (typeof value === 'string') return parseJsonObject(value);
  if (!value || typeof value !== 'object') return null;

  const mcpText = extractMcpText(value);
  if (mcpText !== null) return parseJsonObject(mcpText);

  return Array.isArray(value) ? null : (value as Record<string, unknown>);
};

/** Collapse whitespace/newlines so a value fits on the single-line row. */
const oneLine = (value: unknown): string => {
  if (value == null) return '';
  return String(value).replace(/\s+/g, ' ').trim();
};

/** First scalar (string/number/bool) param value — fallback row arg for unknown tools. */
const firstScalarParam = (obj: Record<string, unknown> | null): string => {
  if (!obj) return '';
  for (const value of Object.values(obj)) {
    const t = typeof value;
    if (t === 'string' || t === 'number' || t === 'boolean') {
      const s = oneLine(value);
      if (s) return s;
    }
  }
  return '';
};

/**
 * Build the one-line row label (verb + primary arg) for a tool call. Known
 * built-in tools get a friendly verb and their most meaningful argument;
 * unknown / MCP tools fall back to the cleaned name plus the first scalar
 * param so the row is never just an opaque tool id.
 */
export const summarizeToolCall = (toolName: string, params: unknown): ToolCallSummary => {
  const name = cleanToolName(toolName || '');
  const obj = asParamsObject(params);
  const get = (key: string): string => oneLine(obj?.[key]);

  switch (name) {
    case 'fs_read':
      return { verb: 'Read', arg: get('path') };
    case 'fs_write':
      return { verb: 'Write', arg: get('path') };
    case 'fs_list':
      return { verb: 'List', arg: get('path') };
    case 'fs_glob':
      return { verb: 'Glob', arg: get('pattern') };
    case 'fs_request_grant':
      return { verb: 'Request access', arg: get('path') };
    case 'bash_exec':
      return { verb: 'Bash', arg: get('command') };
    case 'web_search':
      return { verb: 'Search', arg: get('query') };
    case 'web_fetch':
      return { verb: 'Fetch', arg: get('url') };
    case 'ask_user':
      return { verb: 'Ask', arg: get('question') };
    default:
      return { verb: name || 'tool', arg: firstScalarParam(obj) };
  }
};

const countLines = (text: unknown): number => {
  if (typeof text !== 'string' || text.length === 0) return 0;
  const trimmed = text.replace(/\n$/, '');
  if (trimmed.length === 0) return 0;
  return trimmed.split('\n').length;
};

const arrayLen = (value: unknown): number => (Array.isArray(value) ? value.length : 0);

/**
 * Build the short, right-aligned result hint shown on the row without
 * expanding (e.g. "exit 0", "128 lines", "3 results"). Returns null when there
 * is nothing useful to show (still running, or a tool we have no summary for) —
 * the row then shows a spinner (running) or nothing.
 */
export const summarizeToolResult = (
  toolName: string,
  result: unknown,
  error: string | null | undefined,
  status: string,
): ToolResultSummary | null => {
  const name = cleanToolName(toolName || '');

  // bash exit code is the most useful signal even on failure, so check it
  // before the generic error branch.
  if (name === 'bash_exec') {
    const obj = asPayloadObject(result);
    if (obj && obj.exitCode != null) {
      const code = Number(obj.exitCode);
      return { text: `exit ${code}`, tone: code === 0 ? 'neutral' : 'error' };
    }
    if (error) return { text: 'error', tone: 'error' };
    if (status === 'running') return null;
    return null;
  }

  if (error || status === 'failed') {
    return { text: 'error', tone: 'error' };
  }
  if (status === 'running') return null;

  const obj = asPayloadObject(result);
  switch (name) {
    case 'fs_read': {
      const n = countLines(obj?.content);
      return n ? { text: `${n} ${n === 1 ? 'line' : 'lines'}`, tone: 'neutral' } : null;
    }
    case 'fs_write':
      return { text: 'written', tone: 'neutral' };
    case 'fs_list': {
      const n = arrayLen(obj?.entries);
      return { text: `${n} ${n === 1 ? 'entry' : 'entries'}`, tone: 'neutral' };
    }
    case 'fs_glob': {
      const n = arrayLen(obj?.matches);
      return { text: `${n} ${n === 1 ? 'match' : 'matches'}`, tone: 'neutral' };
    }
    case 'web_search': {
      const n = arrayLen(obj?.results);
      return { text: `${n} ${n === 1 ? 'result' : 'results'}`, tone: 'neutral' };
    }
    case 'web_fetch':
      return { text: 'fetched', tone: 'neutral' };
    case 'ask_user':
      return obj && typeof obj.answer === 'string' ? { text: 'answered', tone: 'neutral' } : null;
    default:
      return null;
  }
};

/**
 * Plain-text view of a tool's result, used for the inline preview and the
 * terminal-style expanded output. Specialises the built-in tools whose result
 * shape we know; falls back to MCP text extraction or pretty JSON.
 */
export const toPreviewText = (
  toolName: string,
  result: unknown,
  error: string | null | undefined,
): string => {
  if (error) return error;
  const name = cleanToolName(toolName || '');
  const obj = asPayloadObject(result);

  if (name === 'bash_exec' && obj) {
    const stdout = typeof obj.stdout === 'string' ? obj.stdout : '';
    const stderr = typeof obj.stderr === 'string' ? obj.stderr : '';
    const combined = [stdout.trimEnd(), stderr.trimEnd()].filter(Boolean).join('\n');
    return combined || '(no output)';
  }

  if ((name === 'fs_read' || name === 'web_fetch') && obj && typeof obj.content === 'string') {
    return obj.content;
  }

  if (name === 'fs_list' && Array.isArray(obj?.entries)) {
    return (obj!.entries as Array<{ path?: string }>)
      .map((e) => (e && typeof e.path === 'string' ? e.path : ''))
      .filter(Boolean)
      .join('\n');
  }

  if (name === 'fs_glob' && Array.isArray(obj?.matches)) {
    return (obj!.matches as Array<{ path?: string }>)
      .map((e) => (e && typeof e.path === 'string' ? e.path : ''))
      .filter(Boolean)
      .join('\n');
  }

  if (name === 'web_search' && Array.isArray(obj?.results)) {
    return (obj!.results as Array<{ title?: string; url?: string }>)
      .map((r) => [r?.title, r?.url].filter(Boolean).join(' — '))
      .filter(Boolean)
      .join('\n');
  }

  const mcp = extractMcpText(result);
  if (mcp) return mcp;

  if (result == null) return '';
  if (typeof result === 'string') return result;
  try {
    return JSON.stringify(result, null, 2);
  } catch {
    return String(result);
  }
};

const EXT_LANG: Record<string, string> = {
  ts: 'ts',
  tsx: 'tsx',
  js: 'js',
  jsx: 'jsx',
  mjs: 'js',
  cjs: 'js',
  json: 'json',
  md: 'markdown',
  markdown: 'markdown',
  py: 'python',
  rs: 'rust',
  go: 'go',
  rb: 'ruby',
  java: 'java',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cc: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  css: 'css',
  scss: 'scss',
  html: 'html',
  xml: 'xml',
  sql: 'sql',
};

/** Best-effort fence language from a file path's extension ('' if unknown). */
export const guessLang = (path: string | undefined): string => {
  if (!path) return '';
  const clean = path.split(/[?#]/)[0]!;
  const dot = clean.lastIndexOf('.');
  if (dot < 0 || dot === clean.length - 1) return '';
  const ext = clean.slice(dot + 1).toLowerCase();
  return EXT_LANG[ext] ?? '';
};
