import React, { useEffect } from 'react';
import { Outlet } from 'react-router-dom';
import TerminalEmulatorWrapper from '../components/TerminalEmulator/TerminalEmulatorWrapper';
import WorkspaceTaskNotifications from '../components/WorkspaceTaskNotifications';
import { CommandProvider } from '../contexts/CommandContext';
import { TabManagerProvider } from '../contexts/TabManagerContext';
import { CommandMessagingProvider } from '../contexts/CommandMessagingContext';
import { ChatManagerProvider } from '../contexts/ChatManagerContext';
import { FleetProvider } from '../contexts/FleetContext';
import { useAssistantEvents } from '../assistant';
import styles from './MainLayout.module.css';

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
        <AssistantEventListener>
          <CommandMessagingProvider>
            <ChatManagerProvider>
              <FleetProvider>
                <div className={styles.mainLayout}>
                  <WorkspaceTaskNotifications />
                  <TerminalEmulatorWrapper />
                  <div className={styles.contentArea}>
                    <Outlet />
                  </div>
                </div>
              </FleetProvider>
            </ChatManagerProvider>
          </CommandMessagingProvider>
        </AssistantEventListener>
      </TabManagerProvider>
    </CommandProvider>
  );
};

export default MainLayout;
