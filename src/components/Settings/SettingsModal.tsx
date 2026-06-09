/**
 * SettingsModal Component
 *
 * Main settings modal with sidebar navigation for different settings sections.
 */

import React, { useState, useEffect, useCallback } from 'react';
import ReactDOM from 'react-dom';
// AgentsSettings was the global-agent CRUD; agents are workspace-local now
// and edited inside each workspace, not from the global Settings modal.
import AssistantProviderSettings from './AssistantProviderSettings';
import McpServersSettings from './McpServersSettings';
import SkillsSettings from './SkillsSettings';
import AppearanceSettings from './AppearanceSettings';
import ApplicationsSettings from './ApplicationsSettings';
import AboutSettings from './AboutSettings';
import styles from './SettingsModal.module.css';

/**
 * Settings icon for the sidebar
 */
const ProviderIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 2a2 2 0 0 1 2 2c0 .74-.4 1.39-1 1.73V7h1a7 7 0 0 1 7 7h1a2 2 0 1 1 0 4h-1v1a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-1H2a2 2 0 1 1 0-4h1a7 7 0 0 1 7-7h1V5.73c-.6-.34-1-.99-1-1.73a2 2 0 0 1 2-2z" />
    <circle cx="8" cy="16" r="1" fill="currentColor" />
    <circle cx="16" cy="16" r="1" fill="currentColor" />
  </svg>
);

// AgentsIcon removed alongside the global Agents tab.

const PlugIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 22v-5" />
    <path d="M9 8V2" />
    <path d="M15 8V2" />
    <path d="M18 8H6v4a6 6 0 0 0 12 0V8z" />
  </svg>
);

const SkillsIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" />
    <path d="M4 4.5A2.5 2.5 0 0 1 6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15z" />
    <path d="M8 7h8" />
    <path d="M8 11h6" />
  </svg>
);

const AppsIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="3" y="3" width="7" height="7" rx="1" />
    <rect x="14" y="3" width="7" height="7" rx="1" />
    <rect x="3" y="14" width="7" height="7" rx="1" />
    <rect x="14" y="14" width="7" height="7" rx="1" />
  </svg>
);

const AppearanceIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="9" />
    <path d="M12 3a9 9 0 0 0 0 18z" fill="currentColor" stroke="none" />
  </svg>
);

const AboutIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="9" />
    <line x1="12" y1="11" x2="12" y2="16" />
    <circle cx="12" cy="8" r="1" fill="currentColor" stroke="none" />
  </svg>
);

/**
 * Close icon
 */
const CloseIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

// Global settings tabs only — agents are workspace-local and edited inside
// the workspace settings, not here.
const TABS = {
  PROVIDER: 'provider',
  SKILLS: 'skills',
  MCP_SERVERS: 'mcp_servers',
  APPLICATIONS: 'applications',
  APPEARANCE: 'appearance',
  ABOUT: 'about',
} as const;

type TabValue = (typeof TABS)[keyof typeof TABS];

interface SettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  initialTab?: TabValue;
}

const SettingsModal = ({ isOpen, onClose, initialTab = TABS.PROVIDER }: SettingsModalProps) => {
  const [activeTab, setActiveTab] = useState<TabValue>(initialTab);

  // Reset to initial tab when modal opens
  useEffect(() => {
    if (isOpen) {
      setActiveTab(initialTab);
    }
  }, [isOpen, initialTab]);

  // Handle escape key
  useEffect(() => {
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && isOpen) {
        onClose();
      }
    };

    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [isOpen, onClose]);

  // Prevent body scroll when modal is open
  useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }
    return () => {
      document.body.style.overflow = '';
    };
  }, [isOpen]);

  const handleOverlayClick = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) {
      onClose();
    }
  }, [onClose]);

  if (!isOpen) {
    return null;
  }

  const renderContent = () => {
    switch (activeTab) {
      case TABS.PROVIDER:
        return <AssistantProviderSettings />;
      case TABS.SKILLS:
        return <SkillsSettings />;
      case TABS.MCP_SERVERS:
        return <McpServersSettings />;
      case TABS.APPLICATIONS:
        return <ApplicationsSettings />;
      case TABS.APPEARANCE:
        return <AppearanceSettings />;
      case TABS.ABOUT:
        return <AboutSettings />;
      default:
        return null;
    }
  };

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className={styles.header}>
          <h2 className={styles.title}>Settings</h2>
          <button className={styles.closeButton} onClick={onClose} title="Close">
            <CloseIcon />
          </button>
        </div>

        <div className={styles.body}>
          {/* Sidebar */}
          <nav className={styles.sidebar}>
            <button
              className={`${styles.navItem} ${activeTab === TABS.PROVIDER ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.PROVIDER)}
            >
              <ProviderIcon />
              <span>AI Provider</span>
            </button>
            <button
              className={`${styles.navItem} ${activeTab === TABS.SKILLS ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.SKILLS)}
            >
              <SkillsIcon />
              <span>Skills</span>
            </button>
            <button
              className={`${styles.navItem} ${activeTab === TABS.MCP_SERVERS ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.MCP_SERVERS)}
            >
              <PlugIcon />
              <span>MCP Servers</span>
            </button>
            <button
              className={`${styles.navItem} ${activeTab === TABS.APPLICATIONS ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.APPLICATIONS)}
            >
              <AppsIcon />
              <span>Applications</span>
            </button>
            <button
              className={`${styles.navItem} ${activeTab === TABS.APPEARANCE ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.APPEARANCE)}
            >
              <AppearanceIcon />
              <span>Appearance</span>
            </button>
            <button
              className={`${styles.navItem} ${activeTab === TABS.ABOUT ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.ABOUT)}
            >
              <AboutIcon />
              <span>About</span>
            </button>
          </nav>

          {/* Content */}
          <div className={styles.content}>
            {renderContent()}
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
};

export default SettingsModal;
export { TABS };
