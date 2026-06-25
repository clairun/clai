import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import { save } from '@tauri-apps/plugin-dialog';
import MarkdownMessage from './Chat/MarkdownMessage';
import { downloadWorkspaceFile, openWorkspacePath, readWorkspaceFile } from '../workspace/client';
import {
  bundleHtmlForPreview,
  isWorkspaceRelativeHref,
  resolveWorkspacePath,
} from '../utils/htmlBundle';
import { openExternal } from '../utils/openExternal';
import type { WorkspaceFileContent, WorkspaceFileEntry } from '../generated/bindings';
import styles from './WorkspaceFilePreviewPanel.module.css';

type HtmlMode = 'preview' | 'source';

// The loaded-file shape this panel renders. `viewer`/`path` mirror the
// WorkspaceFileContent payload from `readWorkspaceFile`; `error` is set
// locally when the read fails.
interface LoadedFile {
  content: string;
  viewer: string;
  path: string;
  error?: string;
}

interface WorkspaceFilePreviewPanelProps {
  workspaceId: string;
  kind: 'memory' | 'artifact';
  entry: WorkspaceFileEntry | null;
  onClose: () => void;
  // Called when a link inside an HTML preview points at another file in the
  // same workspace; receives the resolved workspace-relative path so the
  // parent can swap the previewed artifact. Omit to disable in-app link
  // navigation (links then do nothing rather than escaping the sandbox).
  onNavigate?: (path: string) => void;
}

// ── Syntax-highlighting setup ──────────────────────────────────────────────
// Reuses the same Prism instance + oneLight theme that MarkdownMessage uses
// for fenced code blocks, so standalone file previews and inline markdown
// snippets render with a consistent look.

const PREVIEW_CODE_STYLE: React.CSSProperties = {
  margin: 0,
  padding: '12px 14px',
  fontSize: '12px',
  lineHeight: '1.5',
  borderRadius: '6px',
  background: 'rgba(0, 0, 0, 0.04)',
  border: '1px solid rgba(0, 0, 0, 0.1)',
  overflow: 'auto',
};

const PREVIEW_CODE_TAG_STYLE: React.CSSProperties = {
  fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
  fontSize: '12px',
};

const PREVIEW_LINE_NUMBER_STYLE: React.CSSProperties = {
  minWidth: '2.5em',
  paddingRight: '12px',
  marginRight: '4px',
  color: 'rgba(0, 0, 0, 0.32)',
  textAlign: 'right',
  userSelect: 'none',
  borderRight: '1px solid rgba(0, 0, 0, 0.06)',
};

// Maps file extensions to Prism language identifiers. Keep this list curated
// — every entry corresponds to a Prism grammar already bundled by
// react-syntax-highlighter's default Prism build.
const EXT_TO_LANG: Record<string, string> = {
  // Web
  js: 'javascript', mjs: 'javascript', cjs: 'javascript',
  jsx: 'jsx',
  ts: 'typescript',
  tsx: 'tsx',
  html: 'markup', htm: 'markup', xml: 'markup', svg: 'markup',
  css: 'css', scss: 'scss', sass: 'sass', less: 'less',
  // Backend / systems
  go: 'go',
  rs: 'rust',
  py: 'python',
  rb: 'ruby',
  java: 'java',
  kt: 'kotlin', kts: 'kotlin',
  swift: 'swift',
  c: 'c', h: 'c',
  cpp: 'cpp', cc: 'cpp', cxx: 'cpp', hpp: 'cpp', hxx: 'cpp',
  cs: 'csharp',
  php: 'php',
  lua: 'lua',
  ex: 'elixir', exs: 'elixir',
  hs: 'haskell',
  scala: 'scala',
  // Shell / config
  sh: 'bash', bash: 'bash', zsh: 'bash', fish: 'bash',
  ps1: 'powershell',
  yaml: 'yaml', yml: 'yaml',
  toml: 'toml',
  ini: 'ini',
  json: 'json', jsonc: 'json',
  // Data / proto / queries
  proto: 'protobuf',
  sql: 'sql',
  graphql: 'graphql', gql: 'graphql',
  // Docs & misc
  md: 'markdown', markdown: 'markdown',
  tex: 'latex',
  diff: 'diff', patch: 'diff',
};

// Some files have meaningful names but no extension (Dockerfile, Makefile) —
// or have a leading-dot name (.gitignore, .env). Match by full lowercase
// basename before falling back to extension.
const FILENAME_TO_LANG: Record<string, string> = {
  'dockerfile': 'docker',
  'containerfile': 'docker',
  'makefile': 'makefile',
  'gnumakefile': 'makefile',
  '.bashrc': 'bash',
  '.zshrc': 'bash',
  '.profile': 'bash',
  '.gitignore': 'bash',
  '.dockerignore': 'bash',
  '.gitattributes': 'bash',
  '.editorconfig': 'ini',
  'cmakelists.txt': 'cmake',
  'rakefile': 'ruby',
  'gemfile': 'ruby',
  'go.mod': 'go',
};

const detectLanguage = (path: string | null | undefined): string | null => {
  if (!path) return null;
  const lastSlash = path.lastIndexOf('/');
  const name = (lastSlash === -1 ? path : path.slice(lastSlash + 1)).toLowerCase();
  if (FILENAME_TO_LANG[name]) return FILENAME_TO_LANG[name];
  const dot = name.lastIndexOf('.');
  // Leading-dot files (.env, .prettierrc): treat the part after the dot
  // like an extension so we still get useful coloring.
  if (dot <= 0) {
    const cleaned = name.replace(/^\./, '');
    return EXT_TO_LANG[cleaned] || null;
  }
  return EXT_TO_LANG[name.slice(dot + 1)] || null;
};

const CodeView = ({ content, language }: { content: string; language: string | null }) => (
  <SyntaxHighlighter
    language={language || 'text'}
    style={oneLight}
    showLineNumbers
    wrapLongLines={false}
    customStyle={PREVIEW_CODE_STYLE}
    codeTagProps={{ style: PREVIEW_CODE_TAG_STYLE }}
    lineNumberStyle={PREVIEW_LINE_NUMBER_STYLE}
  >
    {content}
  </SyntaxHighlighter>
);

// postMessage `type` used by the HTML-preview iframe to ask the parent
// to open an external URL in the OS default browser. Keep in sync with
// `EXTERNAL_LINK_INTERCEPTOR_SCRIPT` below.
const EXTERNAL_LINK_MESSAGE_TYPE = 'clai-html-preview-open-external';

// postMessage `type` used when the iframe link points at another file in
// the same workspace (e.g. an index page linking to a report). The parent
// resolves the relative href against the current artifact's directory and
// opens the target as an artifact instead of navigating the iframe (which a
// sandboxed frame can't do) or leaking it to a browser tab.
const INTERNAL_LINK_MESSAGE_TYPE = 'clai-html-preview-open-artifact';

// Script injected into the HTML preview iframe. Captures clicks on `<a>`
// elements, suppresses the iframe's own (sandbox-broken) navigation, and
// posts the target up to the parent, which routes it one of two ways:
//   • external targets (http(s)/mailto/ftp/tel) → OS default browser;
//   • everything else relative (another file in the workspace) → opened as
//     an artifact in the preview panel.
// In-page anchors (`#section`) and `javascript:` URIs are left to the
// browser's native handling.
//
// The script needs `allow-scripts` in the iframe sandbox but deliberately
// runs WITHOUT `allow-same-origin`, so the iframe stays in a unique-origin
// sandbox — it can postMessage to the parent but cannot read the parent's
// DOM, cookies, or storage.
const EXTERNAL_LINK_INTERCEPTOR_SCRIPT = `
<script>
(function () {
  var EXTERNAL_TYPE = ${JSON.stringify(EXTERNAL_LINK_MESSAGE_TYPE)};
  var INTERNAL_TYPE = ${JSON.stringify(INTERNAL_LINK_MESSAGE_TYPE)};
  var EXTERNAL_PROTOCOLS = ['http:', 'https:', 'mailto:', 'ftp:', 'tel:'];

  // Decide how a clicked anchor should be routed, based on the RAW href.
  // We must NOT resolve against document.baseURI: in an about:srcdoc frame
  // the spec resolves relative URLs against the *parent* document, so a
  // sibling link like "report.html" comes back as "http://localhost/report.html"
  // and would look external. The author's raw href is the real intent.
  // Returns null to let the browser handle it natively.
  function classify(anchor) {
    if (!anchor) return null;
    var raw = anchor.getAttribute('href');
    if (!raw) return null;
    raw = raw.trim();
    if (!raw || raw.charAt(0) === '#') return null; // empty or in-page anchor

    // Protocol-relative ("//host/…") is always external.
    if (raw.indexOf('//') === 0) {
      return { type: EXTERNAL_TYPE, url: anchor.href };
    }
    // An explicit URL scheme ("https:", "mailto:", "javascript:", …).
    var scheme = /^([a-z][a-z0-9+.-]*):/i.exec(raw);
    if (scheme) {
      var proto = scheme[1].toLowerCase() + ':';
      if (EXTERNAL_PROTOCOLS.indexOf(proto) !== -1) {
        return { type: EXTERNAL_TYPE, url: anchor.href };
      }
      // javascript:, data:, blob:, file:, unknown — leave to the browser.
      return null;
    }
    // No scheme and not protocol-relative → a relative (or root-absolute)
    // path inside the workspace. Send the RAW href so the parent can resolve
    // it against the current artifact's directory.
    return { type: INTERNAL_TYPE, href: raw };
  }

  function handler(event) {
    var target = event.target;
    if (!target || typeof target.closest !== 'function') return;
    var anchor = target.closest('a');
    var routed = classify(anchor);
    if (!routed) return;
    event.preventDefault();
    try {
      window.parent.postMessage(routed, '*');
    } catch (_) {
      // postMessage shouldn't throw, but the parent might be gone (e.g.
      // the panel was unmounted while the click was in flight). Nothing
      // useful we can do here.
    }
  }

  // Capture phase so we run before any in-document handlers, and so a
  // click on a descendant of <a> still hits us.
  document.addEventListener('click', handler, true);
  document.addEventListener('auxclick', handler, true);
})();
</script>
`;

const augmentHtmlForPreview = (rawHtml: string): string => {
  if (typeof rawHtml !== 'string' || rawHtml.length === 0) return rawHtml;
  // Inject the interceptor just before </body> when present, so the
  // listener attaches after the rest of the document parses. If there's
  // no </body>, append at the end — browsers tolerate the unbalanced
  // body and the script still runs.
  const insertion = `${EXTERNAL_LINK_INTERCEPTOR_SCRIPT}`;
  const bodyClose = rawHtml.lastIndexOf('</body>');
  if (bodyClose !== -1) {
    return rawHtml.slice(0, bodyClose) + insertion + rawHtml.slice(bodyClose);
  }
  return rawHtml + insertion;
};

const formatTimestamp = (timestamp: number | bigint | null | undefined): string => {
  if (!timestamp) return '';
  return new Date(Number(timestamp)).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

const looksLikeMarkdown = (viewer: string | undefined, path: string | undefined): boolean => {
  if (viewer === 'markdown') return true;
  if (!path) return false;
  const lower = path.toLowerCase();
  return lower.endsWith('.md') || lower.endsWith('.markdown');
};

const isJsonLike = (viewer: string | undefined, path: string | undefined): boolean => {
  if (viewer === 'json') return true;
  if (!path) return false;
  return path.toLowerCase().endsWith('.json');
};

const looksLikeHtml = (viewer: string | undefined, path: string | undefined): boolean => {
  if (viewer === 'html') return true;
  if (!path) return false;
  const lower = path.toLowerCase();
  return lower.endsWith('.html') || lower.endsWith('.htm');
};

const renderBody = (
  file: LoadedFile | null,
  htmlMode: HtmlMode,
  htmlBundle: string | null,
  bundling: boolean,
  onMarkdownLinkClick: (event: React.MouseEvent) => void,
) => {
  if (!file) return null;
  if (file.error) {
    return <div className={styles.error}>{file.error}</div>;
  }
  if (!file.content) {
    return <div className={styles.empty}>This file is empty.</div>;
  }
  if (looksLikeMarkdown(file.viewer, file.path)) {
    // Capture-phase so we intercept relative links (sibling workspace files)
    // before the anchor's default opens them in a browser tab. External links
    // fall through to the default target="_blank" handling.
    return (
      <div
        className={styles.markdownBody}
        onClickCapture={onMarkdownLinkClick}
        onAuxClickCapture={onMarkdownLinkClick}
      >
        <MarkdownMessage content={file.content} />
      </div>
    );
  }
  if (looksLikeHtml(file.viewer, file.path) && htmlMode === 'preview') {
    // The bundler inlines any local siblings (stylesheets, scripts, images,
    // fonts) the report references by relative path — `srcDoc` has no base
    // URL, so without this they'd silently fail to load. We wait for it
    // before mounting the iframe so the preview never flashes unstyled.
    if (bundling || htmlBundle === null) {
      return (
        <div className={styles.htmlBody}>
          <div className={styles.empty}>Loading preview…</div>
        </div>
      );
    }
    // `allow-scripts` is required for the injected link-interceptor to
    // run. We intentionally do NOT add `allow-same-origin` — the iframe
    // stays in a unique-origin sandbox, so any artifact JS is isolated
    // from the host app's DOM/storage and can only reach the parent
    // through postMessage (which we filter by `type` below).
    return (
      <div className={styles.htmlBody}>
        <iframe
          className={styles.htmlFrame}
          title={`${file.path} preview`}
          srcDoc={augmentHtmlForPreview(htmlBundle)}
          sandbox="allow-scripts"
          referrerPolicy="no-referrer"
        />
      </div>
    );
  }
  if (looksLikeHtml(file.viewer, file.path)) {
    // Source-mode HTML — render the raw markup with syntax highlighting.
    return <CodeView content={file.content} language="markup" />;
  }
  if (isJsonLike(file.viewer, file.path)) {
    let pretty = file.content;
    try {
      pretty = JSON.stringify(JSON.parse(file.content), null, 2);
    } catch {
      // Leave raw content if parse fails.
    }
    return <CodeView content={pretty} language="json" />;
  }
  return <CodeView content={file.content} language={detectLanguage(file.path)} />;
};

export default function WorkspaceFilePreviewPanel({
  workspaceId,
  kind,
  entry,
  onClose,
  onNavigate,
}: WorkspaceFilePreviewPanelProps) {
  const [file, setFile] = useState<LoadedFile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [htmlMode, setHtmlMode] = useState<HtmlMode>('preview');
  // Self-contained HTML for the preview iframe: the raw markup with every
  // local resource inlined. Null until the bundler resolves for the current
  // file; `bundling` gates the iframe so it never mounts mid-inline.
  const [htmlBundle, setHtmlBundle] = useState<string | null>(null);
  const [bundling, setBundling] = useState(false);
  // `justCopied` flips for ~1.2s after a successful copy so the button can
  // swap its icon/label to a checkmark — purely a visual confirmation, not
  // gating the action. Cleared on file change so reopening a preview
  // resets the affordance.
  const [justCopied, setJustCopied] = useState(false);
  const [downloading, setDownloading] = useState(false);
  // Maximize the preview to fill the conversation area (keeps the artifacts
  // list + workspace rail visible). View-only; resets when the panel closes.
  const [fullscreen, setFullscreen] = useState(false);
  // Which header field was just copied via click-to-copy ('name' = the title,
  // 'path' = the workspace-relative path row). Drives a transient "Copied"
  // tag next to the clicked element; cleared after ~1.2s or on file change.
  const [copiedField, setCopiedField] = useState<'name' | 'path' | null>(null);
  const copiedFieldTimerRef = useRef<number | null>(null);

  const copyField = useCallback(async (field: 'name' | 'path', text: string) => {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopiedField(field);
      if (copiedFieldTimerRef.current) window.clearTimeout(copiedFieldTimerRef.current);
      copiedFieldTimerRef.current = window.setTimeout(() => setCopiedField(null), 1200);
    } catch {
      // Clipboard unavailable — nothing actionable; the text stays visible
      // and selectable on screen.
    }
  }, []);

  useEffect(() => () => {
    if (copiedFieldTimerRef.current) window.clearTimeout(copiedFieldTimerRef.current);
  }, []);

  useEffect(() => {
    if (!entry?.path) {
// eslint-disable-next-line react-hooks/set-state-in-effect -- Async file-content bootstrap: fetch on workspaceId/path/viewer change with cancellation guard; setLoading/setError/setFile is outside the lint's set-state model.
      setLoading(false);
      return undefined;
    }
    const path = entry.path;

    let cancelled = false;
    setLoading(true);
    setError('');
    setFile(null);
    setHtmlBundle(null);

    const load = async () => {
      try {
        const result = (await readWorkspaceFile(workspaceId, path)) as WorkspaceFileContent | null;
        if (cancelled) return;
        setFile({
          content: result?.content || '',
          viewer: result?.viewer || entry.viewer || 'text',
          path,
        });
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Failed to read file.');
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [workspaceId, entry?.path, entry?.viewer]);

  useEffect(() => {
// eslint-disable-next-line react-hooks/set-state-in-effect -- Resets htmlMode/justCopied/copiedField when the entry path changes; 3-field prop→state mirror cannot be expressed as a useMemo without re-rendering on every copy click.
    setHtmlMode('preview');
    setJustCopied(false);
    setCopiedField(null);
  }, [entry?.path]);

  // Build the self-contained preview bundle once a loaded file turns out to
  // be HTML and the user is in preview mode. Runs off the main load so the
  // panel can paint immediately; falls back to the raw markup if inlining
  // fails so a broken asset never blanks the preview.
  useEffect(() => {
    if (!file || file.error || htmlMode !== 'preview' || !looksLikeHtml(file.viewer, file.path)) {
      return undefined;
    }
    let cancelled = false;
// eslint-disable-next-line react-hooks/set-state-in-effect -- Async HTML bundle build (fire-and-forget invoke with cancellation); the lint cannot model the 'loaded file + html mode + looks-like-html' gating that decides whether to build at all.
    setBundling(true);
    setHtmlBundle(null);
    bundleHtmlForPreview(workspaceId, file.path, file.content)
      .then((bundled) => {
        if (!cancelled) setHtmlBundle(bundled);
      })
      .catch(() => {
        if (!cancelled) setHtmlBundle(file.content);
      })
      .finally(() => {
        if (!cancelled) setBundling(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, file, htmlMode]);

  const canAct = Boolean(
    !loading && !error && file && typeof file.content === 'string' && file.content.length > 0,
  );

  const handleCopy = async () => {
    if (!canAct || !file) return;
    try {
      await navigator.clipboard.writeText(file.content);
      setJustCopied(true);
      window.setTimeout(() => setJustCopied(false), 1200);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to copy.');
    }
  };

  const defaultDownloadName = (() => {
    if (entry?.name) return entry.name;
    if (!entry?.path) return 'file';
    const slash = entry.path.lastIndexOf('/');
    return slash === -1 ? entry.path : entry.path.slice(slash + 1);
  })();

  const handleOpenInEditor = async () => {
    if (!entry?.path) return;
    try {
      await openWorkspacePath(workspaceId, entry.path, 'editor');
    } catch (err) {
      setError(
        typeof err === 'string'
          ? err
          : err instanceof Error
            ? err.message
            : 'Failed to open in editor.'
      );
    }
  };

  const handleDownload = async () => {
    if (!canAct || downloading || !entry?.path) return;
    setDownloading(true);
    try {
      const dest = await save({ defaultPath: defaultDownloadName });
      if (!dest) return; // user cancelled
      await downloadWorkspaceFile(workspaceId, entry.path, dest);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to download.');
    } finally {
      setDownloading(false);
    }
  };

  // Route external-link clicks inside the HTML preview iframe through
  // Tauri's opener so they always launch the OS default browser
  // instead of replacing the iframe's content (which is what a sandboxed
  // iframe does for top-level `<a>` clicks by default).
  useEffect(() => {
    const handler = (event: MessageEvent) => {
      const data = event?.data as { type?: string; url?: string } | null;
      if (!data || data.type !== EXTERNAL_LINK_MESSAGE_TYPE) return;
      const { url } = data;
      if (typeof url !== 'string' || url.length === 0) return;
      openExternal(url).catch((err) => {
        // Non-fatal — the user can still copy the URL from the source view.
        console.error('[WorkspaceFilePreviewPanel] Failed to open external URL:', err);
      });
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  }, []);

  // Route in-workspace link clicks (one report linking to another) to the
  // parent so it can swap the previewed artifact. The href is resolved
  // against the file currently on screen, which is what the link is relative
  // to. No-ops without an `onNavigate` handler.
  useEffect(() => {
    const currentPath = file?.path || entry?.path;
    if (!onNavigate || !currentPath) return undefined;
    const handler = (event: MessageEvent) => {
      const data = event?.data as { type?: string; href?: string } | null;
      if (!data || data.type !== INTERNAL_LINK_MESSAGE_TYPE) return;
      const { href } = data;
      if (typeof href !== 'string' || href.length === 0) return;
      const resolved = resolveWorkspacePath(currentPath, href);
      if (resolved) onNavigate(resolved);
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  }, [file?.path, entry?.path, onNavigate]);

  // Markdown previews render in the app DOM (not an iframe), so we intercept
  // link clicks here directly. Relative links point at sibling workspace
  // files — resolve them against the current file and open as an artifact;
  // external links fall through to the anchor's default (OS browser).
  const handleMarkdownLinkClick = useCallback(
    (event: React.MouseEvent) => {
      const anchor = (event.target as HTMLElement | null)?.closest('a');
      if (!anchor) return;
      const href = anchor.getAttribute('href');
      const currentPath = file?.path || entry?.path;
      if (!onNavigate || !currentPath || !isWorkspaceRelativeHref(href)) return;
      const resolved = resolveWorkspacePath(currentPath, href!);
      if (!resolved) return;
      event.preventDefault();
      event.stopPropagation();
      onNavigate(resolved);
    },
    [onNavigate, file?.path, entry?.path],
  );

  if (!entry) return null;

  const kindLabel = kind === 'memory' ? 'Memory' : 'Artifact';
  const isHtml = looksLikeHtml(file?.viewer || entry.viewer, file?.path || entry.path);

  return (
    <aside
      className={`${styles.panel} ${fullscreen ? styles.panelFullscreen : ''}`}
      role="region"
      aria-label={`${kindLabel}: ${entry.name}`}
    >
      <div className={styles.header}>
        <div className={styles.headerLeft}>
          <button
            type="button"
            className={styles.titleButton}
            onClick={() => copyField('name', entry.name)}
            title={copiedField === 'name' ? 'Copied!' : `${entry.name} — click to copy file name`}
          >
            {entry.name}
          </button>
          {copiedField === 'name' && (
            <span className={styles.copiedTag} role="status">Copied</span>
          )}
          <span className={styles.kindPill}>{kindLabel}</span>
        </div>
        <div className={styles.headerActions}>
          <button
            type="button"
            className={styles.iconButton}
            onClick={handleOpenInEditor}
            disabled={!entry?.path}
            title="Open in editor"
            aria-label="Open in editor"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z" />
            </svg>
          </button>
          <button
            type="button"
            className={styles.iconButton}
            onClick={handleCopy}
            disabled={!canAct}
            title={justCopied ? 'Copied!' : 'Copy raw content'}
            aria-label="Copy raw content"
          >
            {justCopied ? (
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="20 6 9 17 4 12" />
              </svg>
            ) : (
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <rect x="9" y="9" width="11" height="11" rx="2" />
                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
              </svg>
            )}
          </button>
          <button
            type="button"
            className={styles.iconButton}
            onClick={handleDownload}
            disabled={!canAct || downloading}
            title={downloading ? 'Saving…' : 'Download file'}
            aria-label="Download file"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="7 10 12 15 17 10" />
              <line x1="12" y1="15" x2="12" y2="3" />
            </svg>
          </button>
          <button
            type="button"
            className={styles.iconButton}
            onClick={() => setFullscreen((f) => !f)}
            title={fullscreen ? 'Restore preview size' : 'Maximize preview'}
            aria-label={fullscreen ? 'Restore preview size' : 'Maximize preview'}
            aria-pressed={fullscreen}
          >
            {fullscreen ? (
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="4 14 10 14 10 20" />
                <polyline points="20 10 14 10 14 4" />
                <line x1="14" y1="10" x2="21" y2="3" />
                <line x1="3" y1="21" x2="10" y2="14" />
              </svg>
            ) : (
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="15 3 21 3 21 9" />
                <polyline points="9 21 3 21 3 15" />
                <line x1="21" y1="3" x2="14" y2="10" />
                <line x1="3" y1="21" x2="10" y2="14" />
              </svg>
            )}
          </button>
          <button
            type="button"
            className={styles.closeButton}
            onClick={onClose}
            title="Close preview"
            aria-label="Close preview"
          >
            ×
          </button>
        </div>
      </div>

      <div className={styles.body}>
        {(entry.path || entry.updatedAt) && (
          <div className={styles.bodyMeta}>
            {entry.path && (
              <>
                <button
                  type="button"
                  className={styles.pathButton}
                  onClick={() => copyField('path', entry.path)}
                  title={copiedField === 'path' ? 'Copied!' : `${entry.path} — click to copy path`}
                >
                  {entry.path}
                </button>
                {copiedField === 'path' && (
                  <span className={styles.copiedTag} role="status">Copied</span>
                )}
              </>
            )}
            {entry.updatedAt && (
              <>
                {entry.path && <span className={styles.sep}>·</span>}
                <span>{formatTimestamp(entry.updatedAt)}</span>
              </>
            )}
            {isHtml && (
              <span className={styles.viewSwitch} role="group" aria-label="HTML view mode">
                <button
                  type="button"
                  className={`${styles.viewSwitchButton} ${htmlMode === 'preview' ? styles.viewSwitchButtonActive : ''}`}
                  onClick={() => setHtmlMode('preview')}
                >
                  Preview
                </button>
                <button
                  type="button"
                  className={`${styles.viewSwitchButton} ${htmlMode === 'source' ? styles.viewSwitchButtonActive : ''}`}
                  onClick={() => setHtmlMode('source')}
                >
                  Source
                </button>
              </span>
            )}
          </div>
        )}
        {loading && <div className={styles.empty}>Loading…</div>}
        {!loading && error && <div className={styles.error}>{error}</div>}
        {!loading && !error && renderBody(file, htmlMode, htmlBundle, bundling, handleMarkdownLinkClick)}
      </div>
    </aside>
  );
}
