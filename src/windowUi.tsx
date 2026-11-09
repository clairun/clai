import { createContext, useContext, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface WindowUi {
  railHidden: boolean;
  setRailHidden: (hidden: boolean) => void;
  toggleRail: () => void;
}

const Context = createContext<WindowUi>({ railHidden: false, setRailHidden: () => {}, toggleRail: () => {} });

export const WindowUiProvider = ({ children }: { children: ReactNode }) => {
  const [railHidden, setRailHidden] = useState(() => new URLSearchParams(window.location.search).has('workspace'));
  return (
    <Context.Provider value={{ railHidden, setRailHidden, toggleRail: () => setRailHidden((value) => !value) }}>
      {children}
    </Context.Provider>
  );
};

export const useWindowUi = () => {
  return useContext(Context);
};

export const openWorkspaceWindow = (workspaceId: string) =>
  invoke<string>('window_open', { workspaceId });
