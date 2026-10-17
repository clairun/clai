import type { ReactNode } from 'react';
import { useNavigate } from 'react-router';
import AppUpdateBadge from './AppUpdateBadge';
import { openGlobalSettings } from '../utils/globalSettings';
import styles from './AppTopBar.module.css';

interface AppTopBarProps {
  /** The other page: `/fleet` or `/workspace`. */
  nav: { label: string; to: string };
  /** The page's counters. Mark one with `data-attention` to highlight it. */
  children: ReactNode;
}

/** Every page's top bar: brand, the button to the other page, counters, the update pill and Settings. */
const AppTopBar = ({ nav, children }: AppTopBarProps) => {
  const navigate = useNavigate();
  return (
    <header className={styles.topBar}>
      <img src="/icon.svg" alt="Clai" className={styles.brandIcon} />
      <button type="button" className={styles.button} onClick={() => navigate(nav.to)}>
        {nav.label}
      </button>
      <div className={styles.counters} role="status" aria-label="Summary">
        {children}
      </div>
      <AppUpdateBadge />
      <button
        type="button"
        className={`${styles.button} ${styles.settingsButton}`}
        onClick={() => openGlobalSettings()}
        title="Settings"
        aria-label="Open settings"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      </button>
    </header>
  );
};

export default AppTopBar;
