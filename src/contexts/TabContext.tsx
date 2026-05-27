/**
 * TabContext
 *
 * Provides tab-specific capability context for MCP selection and custom key/value data.
 */

import React, { createContext, useContext, useState, useEffect, useCallback } from 'react';

interface TabContextInitial {
  mcpServers?: {
    attachedServerIds?: string[];
    selectedServerIds?: string[];
    disabledServerIds?: string[];
  };
  customContext?: Record<string, unknown>;
  assistantConnectionId?: string | null;
}

interface TabContextChange {
  mcpServers: {
    attachedServerIds: string[];
    disabledServerIds: string[];
  };
  assistantConnectionId: string | null;
  customContext: Record<string, unknown>;
}

interface TabContextProviderProps {
  children: React.ReactNode;
  tabId: string;
  initialContext?: TabContextInitial | null;
  onContextChange?: (context: TabContextChange) => void;
}

type StringListUpdater = string[] | ((prev: string[]) => string[]);
type ConnectionIdUpdater = string | null | ((prev: string | null) => string | null);

export interface TabContextValue {
  tabId: string;
  selectedMcpServerIds: string[];
  setSelectedMcpServerIds: (value: StringListUpdater) => void;
  disabledMcpServerIds: string[];
  setDisabledMcpServerIds: (value: StringListUpdater) => void;
  assistantConnectionId: string | null;
  setAssistantConnectionId: (value: ConnectionIdUpdater) => void;
  customContext: Record<string, unknown>;
  setCustomContext: (key: string, value: unknown) => void;
  getCustomContext: (key: string) => unknown;
  deleteCustomContext: (key: string) => void;
  clearCustomContext: () => void;
}

const TabContext = createContext<TabContextValue | null>(null);

export function TabContextProvider({
  children,
  tabId,
  initialContext,
  onContextChange,
}: TabContextProviderProps) {
  const [selectedMcpServerIds, setSelectedMcpServerIds] = useState<string[]>(
    initialContext?.mcpServers?.attachedServerIds || initialContext?.mcpServers?.selectedServerIds || []
  );
  const [disabledMcpServerIds, setDisabledMcpServerIds] = useState<string[]>(
    initialContext?.mcpServers?.disabledServerIds || []
  );
  const [customContext, setCustomContextState] = useState<Record<string, unknown>>(
    initialContext?.customContext || {}
  );
  const [assistantConnectionId, setAssistantConnectionIdState] = useState<string | null>(
    initialContext?.assistantConnectionId || null
  );

  useEffect(() => {
    setSelectedMcpServerIds(
      initialContext?.mcpServers?.attachedServerIds || initialContext?.mcpServers?.selectedServerIds || []
    );
    setDisabledMcpServerIds(initialContext?.mcpServers?.disabledServerIds || []);
    setAssistantConnectionIdState(initialContext?.assistantConnectionId || null);
    setCustomContextState(initialContext?.customContext || {});
  }, [tabId, initialContext]);

  const emitContextChange = useCallback((
    nextMcpServerIds: string[],
    nextDisabledIds: string[],
    nextAssistantConnectionId: string | null,
    nextCustomContext: Record<string, unknown>
  ) => {
    if (!onContextChange) {
      return;
    }

    onContextChange({
      mcpServers: {
        attachedServerIds: nextMcpServerIds,
        disabledServerIds: nextDisabledIds,
      },
      assistantConnectionId: nextAssistantConnectionId,
      customContext: nextCustomContext,
    });
  }, [onContextChange]);

  const updateSelectedMcpServerIds = useCallback((value: StringListUpdater) => {
    setSelectedMcpServerIds((prev) => {
      const nextValue = typeof value === 'function' ? value(prev) : value;
      emitContextChange(nextValue, disabledMcpServerIds, assistantConnectionId, customContext);
      return nextValue;
    });
  }, [assistantConnectionId, customContext, disabledMcpServerIds, emitContextChange]);

  const updateDisabledMcpServerIds = useCallback((value: StringListUpdater) => {
    setDisabledMcpServerIds((prev) => {
      const nextValue = typeof value === 'function' ? value(prev) : value;
      emitContextChange(selectedMcpServerIds, nextValue, assistantConnectionId, customContext);
      return nextValue;
    });
  }, [assistantConnectionId, customContext, emitContextChange, selectedMcpServerIds]);

  const setAssistantConnectionId = useCallback((value: ConnectionIdUpdater) => {
    setAssistantConnectionIdState((prev) => {
      const nextValue = typeof value === 'function' ? value(prev) : value;
      emitContextChange(selectedMcpServerIds, disabledMcpServerIds, nextValue, customContext);
      return nextValue;
    });
  }, [customContext, disabledMcpServerIds, emitContextChange, selectedMcpServerIds]);

  const setCustomContext = useCallback((key: string, value: unknown) => {
    setCustomContextState((prev) => {
      const nextContext = { ...prev, [key]: value };
      emitContextChange(selectedMcpServerIds, disabledMcpServerIds, assistantConnectionId, nextContext);
      return nextContext;
    });
  }, [assistantConnectionId, disabledMcpServerIds, emitContextChange, selectedMcpServerIds]);

  const getCustomContext = useCallback((key: string) => {
    return customContext[key];
  }, [customContext]);

  const deleteCustomContext = useCallback((key: string) => {
    setCustomContextState((prev) => {
      const nextContext = { ...prev };
      delete nextContext[key];
      emitContextChange(selectedMcpServerIds, disabledMcpServerIds, assistantConnectionId, nextContext);
      return nextContext;
    });
  }, [assistantConnectionId, disabledMcpServerIds, emitContextChange, selectedMcpServerIds]);

  const clearCustomContext = useCallback(() => {
    setCustomContextState({});
    emitContextChange(selectedMcpServerIds, disabledMcpServerIds, assistantConnectionId, {});
  }, [assistantConnectionId, disabledMcpServerIds, emitContextChange, selectedMcpServerIds]);

  const value: TabContextValue = {
    tabId,
    selectedMcpServerIds,
    setSelectedMcpServerIds: updateSelectedMcpServerIds,
    disabledMcpServerIds,
    setDisabledMcpServerIds: updateDisabledMcpServerIds,
    assistantConnectionId,
    setAssistantConnectionId,
    customContext,
    setCustomContext,
    getCustomContext,
    deleteCustomContext,
    clearCustomContext,
  };

  return (
    <TabContext.Provider value={value}>
      {children}
    </TabContext.Provider>
  );
}

export function useTabContext(): TabContextValue {
  const context = useContext(TabContext);

  if (!context) {
    throw new Error('useTabContext must be used within a TabContextProvider');
  }

  return context;
}

export default TabContext;
