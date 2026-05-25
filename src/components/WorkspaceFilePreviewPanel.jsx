import React, { useEffect, useState } from 'react';
import MarkdownMessage from './Chat/MarkdownMessage';
import { readWorkspaceFile } from '../workspace/client';
import styles from './WorkspaceFilePreviewPanel.module.css';

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
    return (
      <div className={styles.htmlBody}>
        <iframe
          className={styles.htmlFrame}
          title={`${file.path} preview`}
          srcDoc={file.content}
          sandbox=""
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
