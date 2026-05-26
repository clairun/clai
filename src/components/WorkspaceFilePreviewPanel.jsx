import React, { useEffect, useState } from 'react';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import MarkdownMessage from './Chat/MarkdownMessage';
import { readWorkspaceFile } from '../workspace/client';
import { openExternal } from '../utils/openExternal';
import styles from './WorkspaceFilePreviewPanel.module.css';

// ── Syntax-highlighting setup ──────────────────────────────────────────────
// Reuses the same Prism instance + oneLight theme that MarkdownMessage uses
// for fenced code blocks, so standalone file previews and inline markdown
// snippets render with a consistent look.

const PREVIEW_CODE_STYLE = {
  margin: 0,
  padding: '12px 14px',
  fontSize: '12px',
  lineHeight: '1.5',
  borderRadius: '6px',
  background: 'rgba(0, 0, 0, 0.04)',
  border: '1px solid rgba(0, 0, 0, 0.1)',
  overflow: 'auto',
};

const PREVIEW_CODE_TAG_STYLE = {
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
  fontSize: '12px',
};

const PREVIEW_LINE_NUMBER_STYLE = {
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
const EXT_TO_LANG = {
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
const FILENAME_TO_LANG = {
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

const detectLanguage = (path) => {
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

const CodeView = ({ content, language }) => (
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

// Script injected into the HTML preview iframe. Captures clicks on
// `<a>` elements with http(s)/mailto/ftp targets, suppresses the
// iframe's own navigation, and posts the URL up to the parent so it
// can route it through Tauri's opener (OS default browser). In-page
// anchors (`#section`) and `javascript:` URIs are left alone.
//
// The script needs `allow-scripts` in the iframe sandbox but
// deliberately runs WITHOUT `allow-same-origin`, so the iframe stays
// in a unique-origin sandbox — it can postMessage to the parent but
// cannot read the parent's DOM, cookies, or storage.
const EXTERNAL_LINK_INTERCEPTOR_SCRIPT = `
<script>
(function () {
  var MESSAGE_TYPE = ${JSON.stringify(EXTERNAL_LINK_MESSAGE_TYPE)};
  var EXTERNAL_PROTOCOLS = ['http:', 'https:', 'mailto:', 'ftp:'];

  function shouldRoute(anchor) {
    if (!anchor) return false;
    var href = anchor.getAttribute('href');
    if (!href) return false;
    if (href.charAt(0) === '#') return false;
    try {
      var url = new URL(anchor.href, document.baseURI);
      return EXTERNAL_PROTOCOLS.indexOf(url.protocol) !== -1;
    } catch (_) {
      return false;
    }
  }

  function handler(event) {
    var target = event.target;
    if (!target || typeof target.closest !== 'function') return;
    var anchor = target.closest('a');
    if (!shouldRoute(anchor)) return;
    event.preventDefault();
    try {
      window.parent.postMessage({ type: MESSAGE_TYPE, url: anchor.href }, '*');
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

const augmentHtmlForPreview = (rawHtml) => {
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

const formatTimestamp = (timestamp) => {
  if (!timestamp) return '';
  return new Date(timestamp).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

const looksLikeMarkdown = (viewer, path) => {
  if (viewer === 'markdown') return true;
  if (!path) return false;
  const lower = path.toLowerCase();
  return lower.endsWith('.md') || lower.endsWith('.markdown');
};

const isJsonLike = (viewer, path) => {
  if (viewer === 'json') return true;
  if (!path) return false;
  return path.toLowerCase().endsWith('.json');
};

const looksLikeHtml = (viewer, path) => {
  if (viewer === 'html') return true;
  if (!path) return false;
  const lower = path.toLowerCase();
  return lower.endsWith('.html') || lower.endsWith('.htm');
};

const renderBody = (file, htmlMode) => {
  if (!file) return null;
  if (file.error) {
    return <div className={styles.error}>{file.error}</div>;
  }
  if (!file.content) {
    return <div className={styles.empty}>This file is empty.</div>;
  }
  if (looksLikeMarkdown(file.viewer, file.path)) {
    return (
      <div className={styles.markdownBody}>
        <MarkdownMessage content={file.content} />
      </div>
    );
  }
  if (looksLikeHtml(file.viewer, file.path) && htmlMode === 'preview') {
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
          srcDoc={augmentHtmlForPreview(file.content)}
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

export default function WorkspaceFilePreviewPanel({ workspaceId, kind, entry, onClose }) {
  const [file, setFile] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [htmlMode, setHtmlMode] = useState('preview');

  useEffect(() => {
    if (!entry?.path) {
      setLoading(false);
      return undefined;
    }

    let cancelled = false;
    setLoading(true);
    setError('');
    setFile(null);

    const load = async () => {
      try {
        const result = await readWorkspaceFile(workspaceId, entry.path);
        if (cancelled) return;
        setFile({
          content: result?.content || '',
          viewer: result?.viewer || entry.viewer || 'text',
          path: entry.path,
        });
      } catch (err) {
        if (cancelled) return;
        setError(typeof err === 'string' ? err : err?.message || 'Failed to read file.');
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
    setHtmlMode('preview');
  }, [entry?.path]);

  // Route external-link clicks inside the HTML preview iframe through
  // Tauri's opener so they always launch the OS default browser
  // instead of replacing the iframe's content (which is what a sandboxed
  // iframe does for top-level `<a>` clicks by default).
  useEffect(() => {
    const handler = (event) => {
      const data = event?.data;
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

  if (!entry) return null;

  const kindLabel = kind === 'memory' ? 'Memory' : 'Artifact';
  const isHtml = looksLikeHtml(file?.viewer || entry.viewer, file?.path || entry.path);

  return (
    <aside className={styles.panel} role="region" aria-label={`${kindLabel}: ${entry.name}`}>
      <div className={styles.header}>
        <div className={styles.headerLeft}>
          <span className={styles.title} title={entry.name}>
            {entry.name}
          </span>
          <span className={styles.kindPill}>{kindLabel}</span>
        </div>
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

      <div className={styles.body}>
        {(entry.path || entry.updatedAt) && (
          <div className={styles.bodyMeta}>
            {entry.path && (
              <span className={styles.path} title={entry.path}>
                {entry.path}
              </span>
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
        {!loading && !error && renderBody(file, htmlMode)}
      </div>
    </aside>
  );
}
