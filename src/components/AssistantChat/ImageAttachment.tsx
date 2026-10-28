import { memo, useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import type { ContentPart } from '../../generated/bindings';
import { readWorkspaceFileBase64 } from '../../workspace/client';
import styles from './AssistantChat.module.css';

export type ImagePart = Extract<ContentPart, { type: 'image' }>;

/**
 * An image part's bytes from the workspace image store, as a data URL:
 * `null` while loading, `false` when it can't be read. The fetch fires only
 * when the caller mounts — i.e. when its message scrolls into the
 * virtualized view — so a long history with many images never loads them
 * all at once.
 */
export const useImageSrc = (workspaceId: string, path: string): string | null | false => {
  const [src, setSrc] = useState<string | null | false>(null);
  useEffect(() => {
    let cancelled = false;
    readWorkspaceFileBase64(workspaceId, path)
      .then((bytes) => {
        if (!cancelled) setSrc(`data:${bytes.mime};base64,${bytes.base64}`);
      })
      .catch(() => {
        if (!cancelled) setSrc(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, path]);
  return src;
};

/**
 * Full-screen zoom overlay for a transcript image. Portals to <body> so it
 * escapes the virtualized list's transformed/overflow ancestors; closes on
 * Escape, backdrop click, or the × button. Reuses the already-loaded data URL
 * (no re-fetch). A click in it is its own: React would otherwise carry it,
 * through the portal, to whatever holds the thumbnail.
 */
const ImageLightbox = ({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) => {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  return createPortal(
    <div
      className={styles.imageLightboxBackdrop}
      onClick={(event) => {
        event.stopPropagation();
        if (event.target === event.currentTarget) onClose();
      }}
      role="dialog"
      aria-label={alt}
    >
      <img className={styles.imageLightboxImage} src={src} alt={alt} />
      <button type="button" className={styles.imageLightboxClose} onClick={onClose} aria-label="Close">
        ×
      </button>
    </div>,
    document.body
  );
};

/** A thumbnail of an image part that opens the lightbox. */
const ImageAttachment = memo(({ workspaceId, part }: { workspaceId: string; part: ImagePart }) => {
  const src = useImageSrc(workspaceId, part.path);
  const [zoomed, setZoomed] = useState(false);

  if (src === false) {
    return <div className={styles.imageAttachmentError}>Image unavailable</div>;
  }
  if (!src) {
    return <div className={styles.imageAttachmentLoading} />;
  }
  const alt = part.filename ?? 'attached image';
  return (
    <>
      <button type="button" className={styles.imageAttachmentButton} title="Click to zoom" onClick={() => setZoomed(true)}>
        <img className={styles.imageAttachment} src={src} alt={alt} />
      </button>
      {zoomed && <ImageLightbox src={src} alt={alt} onClose={() => setZoomed(false)} />}
    </>
  );
});
ImageAttachment.displayName = 'ImageAttachment';

export default ImageAttachment;
