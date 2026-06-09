import React, { createContext, useContext, useState, useCallback, useEffect, useRef, useMemo } from 'react';

/**
 * ChatManagerContext
 *
 * Manages chat panel visibility state based on the active tab context.
 * Each tab can keep its own open/close state across tab switches.
 *
 * Note: Message state and streaming is handled by AgentActivityContext.
 * This context only handles the UI visibility of the chat panel.
 */

interface PanelState {
  isOpen: boolean;
}
type PanelStates = Record<string, PanelState>;

interface ChatManagerValue {
  panelStates: PanelStates;
  activeSpaceRoom: string | null;
  setActiveContext: (space: string | null | undefined, room: string | null | undefined) => void;
  toggleChat: () => void;
  openChat: () => void;
  closeChat: () => void;
  isCurrentChatOpen: () => boolean;
  getPanelState: (space: string | null | undefined, room: string | null | undefined) => PanelState;
  clearAllChats: () => void;
  // Legacy aliases for compatibility
  chatInstances: PanelStates;
  getCurrentChatInstance: () => PanelState | null;
  getChatInstance: (space: string | null | undefined, room: string | null | undefined) => PanelState;
}

const ChatManagerContext = createContext<ChatManagerValue | null>(null);

export const useChatManager = (): ChatManagerValue => {
  const context = useContext(ChatManagerContext);
  if (!context) {
    throw new Error('useChatManager must be used within a ChatManagerProvider');
  }
  return context;
};

export const ChatManagerProvider = ({ children }: { children: React.ReactNode }) => {
  // Store panel state by context key.
  // Format: { 'tab-id--context': { isOpen: boolean } }
  const [panelStates, setPanelStates] = useState<PanelStates>({});

  // Track the currently active chat context
  const [activeSpaceRoom, setActiveSpaceRoom] = useState<string | null>(null);

  // Mirror panelStates into a ref so stable callbacks (getPanelState,
  // setActiveContext) always read the latest snapshot without invalidating
  // the useCallback chain. Writing `ref.current` directly during render
  // trips the `react-hooks/refs` lint rule, so the mirror lives in an
  // effect keyed on panelStates.
  const panelStatesRef = useRef(panelStates);
  useEffect(() => {
    panelStatesRef.current = panelStates;
  }, [panelStates]);

  /**
   * Generate a unique key for a chat context
   */
  const generateKey = useCallback((space: string | null | undefined, room: string | null | undefined) => {
    const spaceKey = space || 'no-space';
    const roomKey = room || 'no-room';
    return `${spaceKey}--${roomKey}`;
  }, []);

  /**
   * Get panel state for a specific context
   */
  const getPanelState = useCallback((space: string | null | undefined, room: string | null | undefined) => {
    const key = generateKey(space, room);
    return panelStatesRef.current[key] || { isOpen: false };
  }, [generateKey]);

  /**
   * Set the active chat context
   * This is called when switching tabs or when context changes
   */
  const setActiveContext = useCallback((space: string | null | undefined, room: string | null | undefined) => {
    const key = generateKey(space, room);

    // Initialize panel state if it doesn't exist
    if (!panelStatesRef.current[key]) {
      setPanelStates(prev => ({
        ...prev,
        [key]: { isOpen: false }
      }));
    }

    setActiveSpaceRoom(key);
  }, [generateKey]);

  /**
   * Toggle the chat open/closed state for the active context
   */
  const toggleChat = useCallback(() => {
    if (!activeSpaceRoom) return;

    setPanelStates(prev => ({
      ...prev,
      [activeSpaceRoom]: {
        isOpen: !prev[activeSpaceRoom]?.isOpen
      }
    }));
  }, [activeSpaceRoom]);

  /**
   * Open the chat for the active context
   */
  const openChat = useCallback(() => {
    if (!activeSpaceRoom) return;

    setPanelStates(prev => ({
      ...prev,
      [activeSpaceRoom]: { isOpen: true }
    }));
  }, [activeSpaceRoom]);

  /**
   * Close the chat for the active context
   */
  const closeChat = useCallback(() => {
    if (!activeSpaceRoom) return;

    setPanelStates(prev => ({
      ...prev,
      [activeSpaceRoom]: { isOpen: false }
    }));
  }, [activeSpaceRoom]);

  /**
   * Check if the current chat is open
   */
  const isCurrentChatOpen = useCallback(() => {
    if (!activeSpaceRoom) return false;
    return panelStatesRef.current[activeSpaceRoom]?.isOpen || false;
  }, [activeSpaceRoom]);

  /**
   * Clear all panel states (useful for logout/reset)
   */
  const clearAllChats = useCallback(() => {
    setPanelStates({});
    setActiveSpaceRoom(null);
  }, []);

  const value = useMemo<ChatManagerValue>(() => {
    return {
      panelStates,
      activeSpaceRoom,
      setActiveContext,
      toggleChat,
      openChat,
      closeChat,
      isCurrentChatOpen,
      getPanelState,
      clearAllChats,
      // Legacy alias for compatibility
      chatInstances: panelStates,
      getCurrentChatInstance: () => {
        if (!activeSpaceRoom) return null;
        return panelStatesRef.current[activeSpaceRoom] || null;
      },
      getChatInstance: getPanelState
    };
  }, [
    panelStates,
    activeSpaceRoom,
    setActiveContext,
    toggleChat,
    openChat,
    closeChat,
    isCurrentChatOpen,
    getPanelState,
    clearAllChats,
  ]);

  return (
    <ChatManagerContext.Provider value={value}>
      {children}
    </ChatManagerContext.Provider>
  );
};

export default ChatManagerContext;
