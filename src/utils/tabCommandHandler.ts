/**
 * Tab Command Handler
 *
 * Handles all tab command operations for managing tabs including:
 * - Creating and switching tabs
 * - Closing tabs
 * - Renaming tabs
 * - Listing tabs
 * - Resetting tab layout
 * - Duplicating tabs
 */

import type { ParsedCommand } from './commandParser';
import type { CommandResult } from './contextCommandHandler';

interface TabInfo {
  id: string;
  title: string;
}

/** Tab-manager API the tab handler drives (from useTabManager). */
export interface TabCommandApi {
  tabs: TabInfo[];
  activeTabId: string | null;
  createTab: (title?: string) => TabInfo;
  switchToTab: (tabId: string) => void;
  switchToNextTab: () => string | null;
  switchToPrevTab: () => string | null;
  closeTab: (tabId: string) => void;
  renameTab: (tabId: string, title: string) => void;
  resetTab: (tabId: string) => CommandResult | void;
  duplicateTab: (tabId: string) => TabInfo;
}

/**
 * Handle tab command execution
 */
export function handleTabCommand(
  command: Pick<ParsedCommand, 'args'>,
  tabManager: TabCommandApi
): CommandResult {
  const { args } = command;
  const { positional } = args;

  // If no arguments, create a new tab with default title
  if (positional.length === 0) {
    return handleNewTab([], tabManager);
  }

  const firstArg = positional[0].toLowerCase();

  // Check if first arg is a subcommand
  const subcommands = ['new', 'close', 'rename', 'list', 'reset', 'duplicate', 'next', 'prev'];

  if (subcommands.includes(firstArg)) {
    const subcommand = firstArg;

    switch (subcommand) {
      case 'new':
        return handleNewTab(positional.slice(1), tabManager);

      case 'close':
        return handleCloseTab(positional.slice(1), tabManager);

      case 'rename':
        return handleRenameTab(positional.slice(1), tabManager);

      case 'list':
        return handleListTabs(tabManager);

      case 'reset':
        return handleResetTab(tabManager);

      case 'duplicate':
        return handleDuplicateTab(tabManager);

      case 'next':
        return handleNextTab(tabManager);

      case 'prev':
        return handlePrevTab(tabManager);

      default:
        return {
          success: false,
          message: `Unknown tab subcommand: ${subcommand}\nUsage: tab [new|close|rename|list|reset|duplicate|next|prev] [args...]`,
        };
    }
  }

  // Check if it's a numeric index for switching tabs
  const index = parseInt(firstArg, 10);
  if (!isNaN(index)) {
    return handleSwitchTab(index, tabManager);
  }

  // If not a subcommand or number, show error
  return {
    success: false,
    message: `Invalid tab command. Use "tab new <title>" to create a tab with a custom title.\nUsage: tab [new|close|rename|list|reset|duplicate|next|prev|<index>] [args...]`,
  };
}

/**
 * Handle tab new <title>
 * Create a new tab with custom title or default title
 */
function handleNewTab(args: string[], tabManager: TabCommandApi): CommandResult {
  const { createTab } = tabManager;

  // If no title provided, create with default
  if (args.length === 0) {
    const newTab = createTab();
    return {
      success: true,
      message: `Created new tab: ${newTab.title}`,
    };
  }

  // Create with custom title
  const title = args.join(' ');
  const newTab = createTab(title);
  return {
    success: true,
    message: `Created new tab: ${newTab.title}`,
  };
}

/**
 * Handle tab <index>
 * Switch to tab by numeric index (1-based)
 */
function handleSwitchTab(index: number, tabManager: TabCommandApi): CommandResult {
  const { tabs, switchToTab } = tabManager;

  if (index < 1 || index > tabs.length) {
    return {
      success: false,
      message: `Tab index out of range. Valid range: 1-${tabs.length}`,
    };
  }

  const targetTab = tabs[index - 1];
  switchToTab(targetTab.id);
  return {
    success: true,
    message: `Switched to tab ${index}: ${targetTab.title}`,
  };
}

/**
 * Handle tab next
 * Navigate to next tab
 */
function handleNextTab(tabManager: TabCommandApi): CommandResult {
  const { tabs, switchToNextTab } = tabManager;

  const result = switchToNextTab();
  if (result) {
    const tab = tabs.find((t) => t.id === result);
    return {
      success: true,
      message: `Switched to next tab: ${tab?.title || result}`,
    };
  }
  return {
    success: false,
    message: 'Already at the last tab',
  };
}

/**
 * Handle tab prev
 * Navigate to previous tab
 */
function handlePrevTab(tabManager: TabCommandApi): CommandResult {
  const { tabs, switchToPrevTab } = tabManager;

  const result = switchToPrevTab();
  if (result) {
    const tab = tabs.find((t) => t.id === result);
    return {
      success: true,
      message: `Switched to previous tab: ${tab?.title || result}`,
    };
  }
  return {
    success: false,
    message: 'Already at the first tab',
  };
}

/**
 * Handle tab close [index]
 * Close current tab or specified tab by index
 */
function handleCloseTab(args: string[], tabManager: TabCommandApi): CommandResult {
  const { tabs, activeTabId, closeTab } = tabManager;

  // No arguments - close current tab
  if (args.length === 0) {
    const currentTab = tabs.find((t) => t.id === activeTabId);

    if (tabs.length === 1) {
      return {
        success: false,
        message: 'Cannot close the last tab',
      };
    }

    if (activeTabId) closeTab(activeTabId);
    return {
      success: true,
      message: `Closed tab: ${currentTab?.title || activeTabId}`,
    };
  }

  // Close tab by index
  const indexStr = args.join(' ');
  const index = parseInt(indexStr, 10);

  if (isNaN(index)) {
    return {
      success: false,
      message: 'Usage: tab close [index]',
    };
  }

  if (index < 1 || index > tabs.length) {
    return {
      success: false,
      message: `Tab index out of range. Valid range: 1-${tabs.length}`,
    };
  }

  if (tabs.length === 1) {
    return {
      success: false,
      message: 'Cannot close the last tab',
    };
  }

  const targetTab = tabs[index - 1];
  closeTab(targetTab.id);

  return {
    success: true,
    message: `Closed tab ${index}: ${targetTab.title}`,
  };
}

/**
 * Handle tab rename <title>
 * Rename current tab
 */
function handleRenameTab(args: string[], tabManager: TabCommandApi): CommandResult {
  if (args.length === 0) {
    return {
      success: false,
      message: 'Usage: tab rename <title>',
    };
  }

  const { tabs, activeTabId, renameTab } = tabManager;
  const newTitle = args.join(' ');

  const currentTab = tabs.find((t) => t.id === activeTabId);
  const oldTitle = currentTab?.title || 'Untitled';

  if (activeTabId) renameTab(activeTabId, newTitle);

  return {
    success: true,
    message: `Renamed tab from "${oldTitle}" to "${newTitle}"`,
  };
}

/**
 * Handle tab list
 * List all tabs with their indices
 */
function handleListTabs(tabManager: TabCommandApi): CommandResult {
  const { tabs, activeTabId } = tabManager;

  if (tabs.length === 0) {
    return {
      success: true,
      message: 'No tabs available',
    };
  }

  const lines: string[] = [];
  lines.push('=== Tabs ===');
  lines.push('');

  tabs.forEach((tab, index) => {
    const isActive = tab.id === activeTabId;
    const marker = isActive ? '* ' : '  ';
    const number = `${index + 1}.`;
    lines.push(`${marker}${number.padEnd(4)} ${tab.title}`);
  });

  lines.push('');
  lines.push(`Total: ${tabs.length} tab${tabs.length !== 1 ? 's' : ''}`);

  return {
    success: true,
    message: lines.join('\n'),
  };
}

/**
 * Handle tab reset
 * Reset current tab layout (clear tiles)
 */
function handleResetTab(tabManager: TabCommandApi): CommandResult {
  const { tabs, activeTabId, resetTab } = tabManager;

  const currentTab = tabs.find((t) => t.id === activeTabId);
  if (activeTabId) resetTab(activeTabId);

  return {
    success: true,
    message: `Reset tab layout: ${currentTab?.title || activeTabId}`,
  };
}

/**
 * Handle tab duplicate
 * Duplicate current tab
 */
function handleDuplicateTab(tabManager: TabCommandApi): CommandResult {
  const { tabs, activeTabId, duplicateTab } = tabManager;

  const currentTab = tabs.find((t) => t.id === activeTabId);
  const newTab = activeTabId ? duplicateTab(activeTabId) : null;

  return {
    success: true,
    message: `Duplicated tab "${currentTab?.title || 'Untitled'}" as "${newTab?.title ?? 'Untitled'}"`,
  };
}

/**
 * Check if a command is a tab command
 */
export function isTabCommand(command: { type?: string; name?: string }): boolean {
  return command.type === 'tab' || command.name === 'tab';
}
