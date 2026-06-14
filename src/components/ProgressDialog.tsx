/**
 * ProgressDialog — a minimal blocking modal for an in-flight operation the
 * user can't cancel (e.g. forking a workspace while the backend copies its
 * files). Mirrors ConfirmDialog's overlay/shell visual language but swaps the
 * action buttons for a spinner: it appears while the work runs and is
 * dismissed by the caller (by flipping `isOpen`) the moment the work resolves.
 *
 * Intentionally non-dismissable — no Escape, no overlay-click close — because
 * the operation is already running and there's nothing to cancel.
 */

import React from 'react';
import ReactDOM from 'react-dom';
import styles from './ProgressDialog.module.css';

interface ProgressDialogProps {
  isOpen: boolean;
  title: React.ReactNode;
  body?: React.ReactNode;
}

const ProgressDialog = ({ isOpen, title, body }: ProgressDialogProps) => {
  if (!isOpen) return null;

  return ReactDOM.createPortal(
    <div className={styles.overlay} role="alertdialog" aria-busy="true" aria-live="assertive">
      <div className={styles.modal}>
        <span className={styles.spinner} aria-hidden="true" />
        <div className={styles.text}>
          <h2 className={styles.title}>{title}</h2>
          {body && <p className={styles.body}>{body}</p>}
        </div>
      </div>
    </div>,
    document.body,
  );
};

export default ProgressDialog;
