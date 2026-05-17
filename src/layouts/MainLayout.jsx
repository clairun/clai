import React, { useEffect } from 'react';
import { Outlet } from 'react-router-dom';
import TerminalEmulatorWrapper from '../components/TerminalEmulator/TerminalEmulatorWrapper';
import DesktopChatPanel from '../components/Chat/DesktopChatPanel';
import WorkspaceTaskNotifications from '../components/WorkspaceTaskNotifications';
import { CommandProvider } from '../contexts/CommandContext';
import { TabManagerProvider } from '../contexts/TabManagerContext';
import { CommandMessagingProvider } from '../contexts/CommandMessagingContext';
import { ChatManagerProvider } from '../contexts/ChatManagerContext';
import { AgentActivityProvider } from '../contexts/AgentActivityContext';
import { FleetProvider } from '../contexts/FleetContext';
import { useAgentBridge } from '../agents';
import { useAssistantEvents } from '../assistant';
import styles from './MainLayout.module.css';

const AgentBridgeInitializer = ({ children }) => {
  useAgentBridge();
  return children;
};

const AssistantEventListener = ({ children }) => {
  useAssistantEvents();
  return children;
};

const MainLayout = () => {
  useEffect(() => {
    if (window.hideSplashScreen) {
      window.hideSplashScreen();
    }
  }, []);

  return (
    <CommandProvider>
      <TabManagerProvider>
        <AgentActivityProvider>
          <AgentBridgeInitializer>
            <AssistantEventListener>
              <CommandMessagingProvider>
                <ChatManagerProvider>
                  <FleetProvider>
                    <div className={styles.mainLayout}>
                      <WorkspaceTaskNotifications />
                      <DesktopChatPanel />
                      <TerminalEmulatorWrapper />
                      <div className={styles.contentArea}>
                        <Outlet />
                      </div>
                    </div>
                  </FleetProvider>
                </ChatManagerProvider>
              </CommandMessagingProvider>
            </AssistantEventListener>
          </AgentBridgeInitializer>
        </AgentActivityProvider>
      </TabManagerProvider>
    </CommandProvider>
  );
};

export default MainLayout;
