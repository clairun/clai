import { useEffect } from 'react';
import ReactDOM from 'react-dom';
import styles from './ImageLightbox.module.css';

/**
 * Full-screen zoom overlay for a transcript image. Portals to <body> so it
 * escapes the virtualized list's transformed/overflow ancestors; closes on
 * Escape, backdrop click, or the × button. Reuses the already-loaded data URL
 * (no re-fetch). A click in it is its own: React would otherwise carry it,
 * through the portal, to whatever holds the image.
 */
const ImageLightbox = ({
  src,
  alt,
  onClose,
}: {
  src: string;
  alt: string;
  onClose: () => void;
}) => {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  return ReactDOM.createPortal(
    <div
      className={styles.backdrop}
      onClick={(event) => {
        event.stopPropagation();
        if (event.target === event.currentTarget) onClose();
      }}
      role="dialog"
      aria-label={alt}
    >
      <img className={styles.image} src={src} alt={alt} />
      <button
        type="button"
        className={styles.close}
        onClick={onClose}
        aria-label="Close"
      >
        ×
      </button>
    </div>,
    document.body
  );
};
ImageLightbox.displayName = 'ImageLightbox';

export default ImageLightbox;
