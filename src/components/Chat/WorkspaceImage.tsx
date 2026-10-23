import { memo, useEffect, useState } from 'react';
import { readWorkspaceFileBase64 } from '../../workspace/client';
import { resolveWorkspacePath } from '../../utils/htmlBundle';
import { useWorkspaceFileLocation } from './WorkspaceFileContext';
import ImageLightbox from './ImageLightbox';
import styles from './WorkspaceImage.module.css';

/**
 * A workspace image embedded in markdown (`![shot](screens/1.png)`). The
 * WebView has no origin a relative `src` could resolve against, so the file
 * is resolved like a `.vl.json` chart link (relative to the enclosing
 * document, leading `/` = workspace root; see WorkspaceFileContext), read
 * through the workspace file API and shown as a data URL. Click to zoom.
 *
 * Renders only phrasing content: markdown puts images inside <p>.
 */

interface LoadedImage {
  path: string;
  src: string | null;
  error: string | null;
}

interface WorkspaceImageProps {
  /** Image path as written in the markdown, relative to the enclosing document. */
  path: string;
  alt?: string;
  title?: string;
}

const WorkspaceImage = memo(({ path, alt = '', title }: WorkspaceImageProps) => {
  const location = useWorkspaceFileLocation();
  const workspaceId = location?.workspaceId;
  const resolvedPath = resolveWorkspacePath(location?.basePath ?? '', path);
  const [image, setImage] = useState<LoadedImage | null>(null);
  const [zoomed, setZoomed] = useState(false);

  useEffect(() => {
    if (!workspaceId) return undefined;
    let cancelled = false;
    readWorkspaceFileBase64(workspaceId, resolvedPath)
      .then(({ mime, base64 }) => {
        if (!cancelled) setImage({ path: resolvedPath, src: `data:${mime};base64,${base64}`, error: null });
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setImage({ path: resolvedPath, src: null, error: err instanceof Error ? err.message : String(err) });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, resolvedPath]);

  const current = image?.path === resolvedPath ? image : null;
  const error = !workspaceId
    ? `Cannot load ${path}: no workspace to read it from`
    : current?.error ?? null;

  if (error) {
    return (
      <span className={styles.error}>
        <span className={styles.errorLabel}>Image failed to load</span>
        <span className={styles.errorMessage}>{error}</span>
      </span>
    );
  }
  if (!current?.src) {
    return <span className={styles.loading} aria-busy="true">Loading image…</span>;
  }
  const src = current.src;
  // The image doubles as a zoom button, which needs an accessible name.
  const label = alt || path;
  return (
    <>
      <img
        className={styles.image}
        src={src}
        alt={label}
        title={title ?? 'Click to zoom'}
        role="button"
        tabIndex={0}
        onClick={() => setZoomed(true)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            setZoomed(true);
          }
        }}
        // The bytes arrived but are not something the WebView can decode
        // (e.g. a non-image file); say so instead of a broken-image box.
        onError={() => setImage({ path: resolvedPath, src: null, error: `${path} is not a displayable image` })}
      />
      {zoomed && <ImageLightbox src={src} alt={label} onClose={() => setZoomed(false)} />}
    </>
  );
});

WorkspaceImage.displayName = 'WorkspaceImage';

export default WorkspaceImage;
