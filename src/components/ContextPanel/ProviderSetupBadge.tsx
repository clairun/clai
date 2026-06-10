import React from 'react';
import { openGlobalSettings } from '../../utils/globalSettings';
import styles from './ProviderSetupBadge.module.css';

/**
 * ProviderSetupBadge — shown in the chat context bar instead of the provider
 * picker when no enabled provider connection exists (typically first run).
 * Same pill silhouette as the picker, but in the critical color so a fresh
 * install has one obvious next step: click it to land directly in
 * Settings → AI Provider with the "Add Connection" form open.
 */
const ProviderSetupBadge = () => (
  <button
    type="button"
    className={styles.badge}
    onClick={() => openGlobalSettings({ tab: 'provider', providerAction: 'new' })}
    title="No AI provider is configured yet — click to add one"
  >
    <svg className={styles.icon} width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
      <line x1="12" y1="9" x2="12" y2="13" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
    <span className={styles.label}>Configure a provider first</span>
  </button>
);

export default ProviderSetupBadge;
