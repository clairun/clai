import { useEffect } from 'react';
import { BrowserRouter as Router } from 'react-router-dom';
import { listen } from '@tauri-apps/api/event';
import AppRoutes from './Routes';
import { usePlatform } from './hooks/usePlatform';
import { useWorkspaceStore, saveWorkspaceStateNow } from './stores/workspaceStore';

function App() {
  const { os } = usePlatform();

  useEffect(() => {
    // Apply desktop platform class to the document root
    document.documentElement.setAttribute('data-platform', os);
    document.documentElement.setAttribute('data-device-type', 'desktop');

    // Also add as classes for easier CSS targeting
    document.documentElement.classList.add(`platform-${os}`);
    document.documentElement.classList.add('device-desktop');
  }, [os]);

  // Initialize workspace store from SQLite on startup
  useEffect(() => {
    // Small delay to ensure database is initialized on the Rust side
    const initTimeout = setTimeout(() => {
      useWorkspaceStore.getState().initialize();
    }, 100);

    // Save immediately on app close (don't rely on debounce)
    const setupCloseHandler = async () => {
      const unlisten = await listen('tauri://close-requested', async () => {
        const state = useWorkspaceStore.getState();
        await saveWorkspaceStateNow(state);
      });
      return unlisten;
    };

    const unlistenPromise = setupCloseHandler();

    return () => {
      clearTimeout(initTimeout);
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return (
    <Router>
      <AppRoutes />
    </Router>
  );
}

export default App;
