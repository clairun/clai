/**
 * SettingsModal Component
 *
 * Main settings modal with sidebar navigation for different settings sections.
 */

import React, { useState, useCallback } from 'react';
import ReactDOM from 'react-dom';
import AgentLibrarySettings from './AgentLibrarySettings';
import AssistantProviderSettings from './AssistantProviderSettings';
import McpServersSettings from './McpServersSettings';
import SkillsSettings from './SkillsSettings';
import AppearanceSettings from './AppearanceSettings';
import ApplicationsSettings from './ApplicationsSettings';
import AboutSettings from './AboutSettings';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';
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

const AgentsIcon = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="9" cy="8" r="3" />
    <path d="M3 20a6 6 0 0 1 12 0" />
    <path d="M16 5a3 3 0 0 1 0 6" />
    <path d="M18 20a6 6 0 0 0-3-5.2" />
  </svg>
);

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

// App-level settings. The Agents tab owns shared teammate definitions; a
// workspace's own Main agent is edited in that workspace's settings.
const TABS = {
  PROVIDER: 'provider',
  AGENTS: 'agents',
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
  // When 'new', the provider tab opens with its "Add Connection" form
  // already open — used by first-run deep links (no provider configured).
  initialProviderAction?: 'new' | null;
  // When set, the agents tab opens with this shared definition in its editor
  // — used by the link on a crew member in workspace settings.
  initialAgentDefinitionId?: string | null;
}

const SettingsModal = ({
  isOpen,
  onClose,
  initialTab = TABS.PROVIDER,
  initialProviderAction = null,
  initialAgentDefinitionId = null,
}: SettingsModalProps) => {
  const [activeTab, setActiveTab] = useState<TabValue>(initialTab);

  // Reset to `initialTab` whenever the modal transitions to open, or the
  // requested tab changes while it is open. The caller (FleetLayout) renders
  // this modal unconditionally and only toggles `isOpen`, so the instance
  // stays mounted across open/close — returning `null` while closed does not
  // unmount it, and `useState(initialTab)` runs only on first mount. We adjust
  // state during render (React's documented "information from a previous
  // render" pattern) rather than in an effect: it skips the extra commit an
  // effect would cause and stays clear of react-hooks/set-state-in-effect.
  const [prevOpen, setPrevOpen] = useState(isOpen);
  const [prevInitialTab, setPrevInitialTab] = useState(initialTab);
  if (isOpen !== prevOpen || initialTab !== prevInitialTab) {
    setPrevOpen(isOpen);
    setPrevInitialTab(initialTab);
    if (isOpen) {
      setActiveTab(initialTab);
    }
  }

  // Escape and the body-scroll lock, shared with every other open overlay:
  // this modal can sit over the workspace Settings modal, and a form modal can
  // sit over it.
  useOverlayLayer(isOpen, onClose);

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
        return <AssistantProviderSettings initialAction={initialProviderAction} />;
      case TABS.AGENTS:
        // The library consumes its initial id once per mount, like the
        // provider tab's initial action. Every deep link comes from a control
        // this modal's overlay covers, so a second one can only arrive after
        // a close, which unmounts the tab.
        return <AgentLibrarySettings initialAgentDefinitionId={initialAgentDefinitionId} />;
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
              className={`${styles.navItem} ${activeTab === TABS.AGENTS ? styles.active : ''}`}
              onClick={() => setActiveTab(TABS.AGENTS)}
            >
              <AgentsIcon />
              <span>Agents</span>
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
