import React, { useEffect, useState } from 'react';
import MarkdownMessage from './Chat/MarkdownMessage';
import { readWorkspaceFile } from '../workspace/client';
import { openExternal } from '../utils/openExternal';
import styles from './WorkspaceFilePreviewPanel.module.css';

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
  if (isJsonLike(file.viewer, file.path)) {
    let pretty = file.content;
    try {
      pretty = JSON.stringify(JSON.parse(file.content), null, 2);
    } catch {
      // Leave raw content if parse fails.
    }
    return <pre className={styles.codeBody}>{pretty}</pre>;
  }
  return <pre className={styles.codeBody}>{file.content}</pre>;
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
