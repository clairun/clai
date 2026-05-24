/**
 * ConfirmDialog — a small focused confirmation modal for destructive
 * actions. Mirrors the visual language of WorkspaceSettingsModal (overlay,
 * rounded shell, primary/secondary/danger buttons) but keeps the surface
 * intentionally minimal: title, body, cancel + confirm. Use for any
 * action where the user clicking through accidentally is worse than the
 * extra friction of one more click.
 */

import React, { useCallback, useEffect } from 'react';
import ReactDOM from 'react-dom';
import styles from './ConfirmDialog.module.css';

const ConfirmDialog = ({
  isOpen,
  title,
  body,
  confirmLabel = 'Confirm',
  cancelLabel = 'Cancel',
  // 'danger' renders confirm as a red danger button; 'primary' renders it
  // as the standard accent. Use 'danger' for delete / drop / discard.
  confirmTone = 'primary',
  busy = false,
  onConfirm,
  onCancel,
}) => {
  // Close on Escape — matches the broader modal idiom in the app.
  // Intentionally no global Enter-binding: for destructive actions the
  // confirm button must be clicked deliberately. Enter-to-cancel happens
  // naturally because Cancel is the autofocused button.
  useEffect(() => {
    if (!isOpen) return undefined;
    const onKey = (e) => {
      if (e.key === 'Escape' && !busy) onCancel?.();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [isOpen, busy, onCancel]);

  const handleOverlay = useCallback((e) => {
    if (e.target === e.currentTarget && !busy) onCancel?.();
  }, [onCancel, busy]);

  if (!isOpen) return null;

  const confirmClass = confirmTone === 'danger'
    ? styles.dangerButton
    : styles.primaryButton;

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={handleOverlay}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        <h2 className={styles.title}>{title}</h2>
        <div className={styles.body}>{body}</div>
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={onCancel}
            disabled={busy}
            autoFocus
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className={confirmClass}
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? 'Working…' : confirmLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
};

export default ConfirmDialog;
