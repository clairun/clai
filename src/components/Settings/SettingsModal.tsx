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
import shell from './SettingsShell.module.css';
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

const NAV: { tab: TabValue; label: string; Icon: () => React.JSX.Element }[] = [
  { tab: TABS.PROVIDER, label: 'AI Provider', Icon: ProviderIcon },
  { tab: TABS.AGENTS, label: 'Agents', Icon: AgentsIcon },
  { tab: TABS.SKILLS, label: 'Skills', Icon: SkillsIcon },
  { tab: TABS.MCP_SERVERS, label: 'MCP Servers', Icon: PlugIcon },
  { tab: TABS.APPLICATIONS, label: 'Applications', Icon: AppsIcon },
  { tab: TABS.APPEARANCE, label: 'Appearance', Icon: AppearanceIcon },
  { tab: TABS.ABOUT, label: 'About', Icon: AboutIcon },
];

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

// Mounted only while open, and afresh for each tab or agent a caller asks
// for, the way workspace settings is: the first frame is the requested tab.
const SettingsModal = ({ isOpen, ...props }: SettingsModalProps) => (
  isOpen ? <SettingsWindow key={`${props.initialTab}:${props.initialAgentDefinitionId ?? ''}`} {...props} /> : null
);

const SettingsWindow = ({
  onClose,
  initialTab = TABS.PROVIDER,
  initialProviderAction = null,
  initialAgentDefinitionId = null,
}: Omit<SettingsModalProps, 'isOpen'>) => {
  const [activeTab, setActiveTab] = useState<TabValue>(initialTab);
  // Tabs are a `switch`, so coming back to Agents mounts a fresh library;
  // the deep link is cleared on any tab change so that remount shows the
  // gallery, not the editor the user navigated away from.
  const [deepLinkAgentId, setDeepLinkAgentId] = useState(initialAgentDefinitionId);

  const selectTab = useCallback((tab: TabValue) => {
    setActiveTab(tab);
    setDeepLinkAgentId(null);
  }, []);

  // Escape and the body-scroll lock, shared with every other open overlay:
  // this modal can sit over the workspace Settings modal, and a form modal can
  // sit over it.
  useOverlayLayer(true, onClose);

  const handleOverlayClick = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) {
      onClose();
    }
  }, [onClose]);

  const renderContent = () => {
    switch (activeTab) {
      case TABS.PROVIDER:
        return <AssistantProviderSettings initialAction={initialProviderAction} />;
      case TABS.AGENTS:
        return <AgentLibrarySettings initialAgentDefinitionId={deepLinkAgentId} />;
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
    <div className={`${shell.overlay} ${shell.overlayNested}`} onClick={handleOverlayClick}>
      <div
        className={shell.window}
        role="dialog"
        aria-modal="true"
        aria-label="Settings"
        onClick={(e) => e.stopPropagation()}
      >
        <nav className={shell.sidebar} aria-label="Settings pages">
          <div className={shell.sidebarTitle}>
            <span className={shell.sidebarHeading}>Settings</span>
            <span className={shell.sidebarCaption}>App settings</span>
          </div>
          <ul className={shell.navList}>
            {NAV.map(({ tab, label, Icon }) => (
              <li key={tab}>
                <button
                  type="button"
                  className={shell.navItem}
                  aria-current={tab === activeTab ? 'page' : undefined}
                  // Focus starts inside the dialog, on the page it opened on.
                  autoFocus={tab === initialTab}
                  onClick={() => selectTab(tab)}
                >
                  <span className={shell.navIcon}><Icon /></span>
                  <span className={shell.navText}>
                    <span className={shell.navLabel}>{label}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <div className={shell.main}>
          <header className={shell.header}>
            <div className={shell.headerText}>
              <h2 className={shell.title}>{NAV.find((item) => item.tab === activeTab)?.label}</h2>
            </div>
            <button type="button" className={shell.closeButton} onClick={onClose} aria-label="Close settings">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </header>

          <div className={shell.body}>
            <div className={styles.pane}>{renderContent()}</div>
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
};

export default SettingsModal;
export { TABS };
