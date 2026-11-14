import { createContext, useCallback, useContext, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface WindowUi {
  railHidden: boolean;
  showRail: () => void;
  toggleRail: () => void;
}

const Context = createContext<WindowUi>({ railHidden: false, showRail: () => {}, toggleRail: () => {} });

export const WindowUiProvider = ({ children }: { children: ReactNode }) => {
  const [railHidden, setRailHidden] = useState(() => new URLSearchParams(window.location.search).has('workspace'));
  const showRail = useCallback(() => setRailHidden(false), []);
  return (
    <Context.Provider value={{ railHidden, showRail, toggleRail: () => setRailHidden((value) => !value) }}>
      {children}
    </Context.Provider>
  );
};

export const useWindowUi = () => {
  return useContext(Context);
};

export const openWorkspaceWindow = (workspaceId: string) =>
  invoke<string>('window_open', { workspaceId });
