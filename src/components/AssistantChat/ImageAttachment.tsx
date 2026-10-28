import { memo, useEffect, useState } from 'react';
import type { ContentPart } from '../../generated/bindings';
import { readWorkspaceFileBase64 } from '../../workspace/client';
import ImageLightbox from '../Chat/ImageLightbox';
import styles from './AssistantChat.module.css';

export type ImagePart = Extract<ContentPart, { type: 'image' }>;

/** How many images stay read; a part's file never changes, so a hit is current. */
const KEPT_IMAGES = 32;

type ImageRead = { url: Promise<string>; done?: string };
const images = new Map<string, ImageRead>();
const imageKey = (workspaceId: string, path: string) => `${workspaceId}\n${path}`;

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
      (bytes) => (read.done = `data:${bytes.mime};base64,${bytes.base64}`),
      (error: unknown) => {
        if (images.get(key) === read) images.delete(key);
        throw error;
      }
    ),
  };
  images.set(key, read);
  if (images.size > KEPT_IMAGES) images.delete(images.keys().next().value!);
  return read;
};

/**
 * An image part's bytes from the workspace image store, as a data URL:
 * `null` while loading, `false` when it can't be read. The read fires only
 * when the caller first mounts — i.e. when its message scrolls into the
 * virtualized view — so a long history with many images never loads them
 * all at once, and a remount shows the image it already read.
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
