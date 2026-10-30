import { memo, useEffect, useState } from 'react';
import type { ImagePart } from '../../assistant/imageParts';
import { readWorkspaceFileBase64 } from '../../workspace/client';
import ImageLightbox from '../Chat/ImageLightbox';
import styles from './AssistantChat.module.css';

/** How much read image data stays kept, in data-URL characters; a part's file never changes, so a hit is current. */
const KEPT_CHARS = 64 * 1024 * 1024;

type ImageRead = { url: Promise<string>; done?: string };
const images = new Map<string, ImageRead>();
const imageKey = (workspaceId: string, path: string) => `${workspaceId}\n${path}`;

/** Drops the reads used longest ago until what is kept fits `KEPT_CHARS`. */
const keepWithinBudget = () => {
  let kept = 0;
  for (const [key, read] of [...images].reverse()) {
    kept += read.done?.length ?? 0;
    if (kept > KEPT_CHARS) images.delete(key);
  }
};

/** Reads a part's file once; later callers share the read or its result. */
const readImage = (workspaceId: string, path: string): ImageRead => {
  const key = imageKey(workspaceId, path);
  const kept = images.get(key);
  if (kept) {
    images.delete(key);
    images.set(key, kept);
    return kept;
  }
  const read: ImageRead = {
    url: readWorkspaceFileBase64(workspaceId, path).then(
      (bytes) => {
        read.done = `data:${bytes.mime};base64,${bytes.base64}`;
        keepWithinBudget();
        return read.done;
      },
      (error: unknown) => {
        if (images.get(key) === read) images.delete(key);
        throw error;
      }
    ),
  };
  images.set(key, read);
  return read;
};

/**
 * An image part's bytes from the workspace image store, as a data URL:
 * `null` while loading, `false` when it can't be read. Reads on mount;
 * remounts reuse the kept read.
 */
export const useImageSrc = (workspaceId: string, path: string): string | null | false => {
  const [src, setSrc] = useState<string | null | false>(() => images.get(imageKey(workspaceId, path))?.done ?? null);
  useEffect(() => {
    let cancelled = false;
    readImage(workspaceId, path)
      .url.then((url) => {
        if (!cancelled) setSrc(url);
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
