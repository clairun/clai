/**
 * Context Command Handler
 *
 * Handles all ctx command operations for managing tab context including:
 * - MCP server visibility
 * - Custom context key-value pairs
 * - Array-based context values
 */

import type { ParsedCommand } from './commandParser';

export interface CommandResult {
  success: boolean;
  message: string;
}

/** Tab-context API the ctx handler reads/writes (from useTabContext). */
export interface ContextCommandApi {
  selectedMcpServerIds?: string[];
  disabledMcpServerIds?: string[];
  customContext: Record<string, unknown>;
  setCustomContext: (key: string, value: unknown) => void;
  deleteCustomContext: (key: string) => void;
}

/**
 * Handle ctx command execution
 */
export function handleContextCommand(
  command: Pick<ParsedCommand, 'args'>,
  tabContext: ContextCommandApi
): CommandResult {
  const { args } = command;
  const { positional } = args;

  // If no subcommand, print current context
  if (positional.length === 0) {
    return printContext(tabContext);
  }

  const subcommand = positional[0].toLowerCase();

  switch (subcommand) {
    case 'space':
    case 'set':
      return handleSetContext(positional.slice(1), tabContext);

    case 'add':
      return handleAddContext(positional.slice(1), tabContext);

    case 'del':
    case 'delete':
      return handleDeleteContext(positional.slice(1), tabContext);

    default:
      return {
        success: false,
        message: `Unknown ctx subcommand: ${subcommand}\nUsage: ctx [set|add|del] [args...]`,
      };
  }
}

/**
 * Print current context
 */
function printContext(tabContext: ContextCommandApi): CommandResult {
  const { selectedMcpServerIds, disabledMcpServerIds, customContext } = tabContext;

  const lines: string[] = [];

  lines.push('=== Current Context ===');
  lines.push('');

  lines.push('MCP Servers:');
  if ((selectedMcpServerIds || []).length === 0) {
    lines.push('  Attached: (none)');
  } else {
    lines.push(`  Attached: ${selectedMcpServerIds!.join(', ')}`);
  }

  if ((disabledMcpServerIds || []).length === 0) {
    lines.push('  Disabled: (none)');
  } else {
    lines.push(`  Disabled: ${disabledMcpServerIds!.join(', ')}`);
  }

  lines.push('');

  // Custom Context
  lines.push('Custom Context:');
  const contextKeys = Object.keys(customContext);
  if (contextKeys.length === 0) {
    lines.push('  (empty)');
  } else {
    contextKeys.sort().forEach((key) => {
      const value = customContext[key];
      if (Array.isArray(value)) {
        lines.push(`  ${key}: [${value.join(', ')}]`);
      } else if (typeof value === 'object' && value !== null) {
        lines.push(`  ${key}: ${JSON.stringify(value)}`);
      } else {
        lines.push(`  ${key}: ${String(value)}`);
      }
    });
  }

  return {
    success: true,
    message: lines.join('\n'),
  };
}

/**
 * Handle ctx set <key> <value>
 */
function handleSetContext(args: string[], tabContext: ContextCommandApi): CommandResult {
  if (args.length < 2) {
    return {
      success: false,
      message: 'Usage: ctx set <key> <value>',
    };
  }

  const key = args[0];
  const value = args.slice(1).join(' ');

  const { setCustomContext } = tabContext;

  // Try to parse as JSON if it looks like JSON
  let parsedValue: unknown = value;
  if (
    (value.startsWith('{') && value.endsWith('}')) ||
    (value.startsWith('[') && value.endsWith(']'))
  ) {
    try {
      parsedValue = JSON.parse(value);
    } catch {
      // Keep as string if JSON parsing fails
    }
  }

  setCustomContext(key, parsedValue);

  return {
    success: true,
    message: `Set ${key} = ${Array.isArray(parsedValue) ? `[${parsedValue.join(', ')}]` : String(parsedValue)}`,
  };
}

/**
 * Handle ctx add <key> <value>
 * Adds value to key (converts to array if needed)
 */
function handleAddContext(args: string[], tabContext: ContextCommandApi): CommandResult {
  if (args.length < 2) {
    return {
      success: false,
      message: 'Usage: ctx add <key> <value>',
    };
  }

  const key = args[0];
  const value = args.slice(1).join(' ');

  const { customContext, setCustomContext } = tabContext;

  const currentValue = customContext[key];

  let newValue: unknown;
  if (currentValue === undefined) {
    // Key doesn't exist, create as single value
    newValue = value;
  } else if (Array.isArray(currentValue)) {
    // Already an array, append
    newValue = [...currentValue, value];
  } else {
    // Convert to array with both values
    newValue = [currentValue, value];
  }

  setCustomContext(key, newValue);

  return {
    success: true,
    message: `Added ${value} to ${key}${Array.isArray(newValue) ? ` (now: [${newValue.join(', ')}])` : ''}`,
  };
}

/**
 * Handle ctx del <key> [value]
 * If value provided, removes value from array
 * If no value, deletes entire key
 */
function handleDeleteContext(args: string[], tabContext: ContextCommandApi): CommandResult {
  if (args.length === 0) {
    return {
      success: false,
      message: 'Usage: ctx del <key> [value]',
    };
  }

  const key = args[0];
  const { customContext, setCustomContext, deleteCustomContext } = tabContext;

  if (!(key in customContext)) {
    return {
      success: false,
      message: `Key not found: ${key}`,
    };
  }

  // If no value specified, delete entire key
  if (args.length === 1) {
    deleteCustomContext(key);
    return {
      success: true,
      message: `Deleted key: ${key}`,
    };
  }

  // Value specified, remove from array
  const value = args.slice(1).join(' ');
  const currentValue = customContext[key];

  if (!Array.isArray(currentValue)) {
    // If it's not an array, check if it matches and delete if so
    if (currentValue === value) {
      deleteCustomContext(key);
      return {
        success: true,
        message: `Deleted ${key} (value matched)`,
      };
    } else {
      return {
        success: false,
        message: `Value ${value} not found in ${key}`,
      };
    }
  }

  // Remove value from array
  const newValue = currentValue.filter((v) => v !== value);

  if (newValue.length === currentValue.length) {
    return {
      success: false,
      message: `Value ${value} not found in ${key}`,
    };
  }

  if (newValue.length === 0) {
    // Array is now empty, delete key
    deleteCustomContext(key);
    return {
      success: true,
      message: `Deleted ${key} (array now empty)`,
    };
  } else if (newValue.length === 1) {
    // Array has one element, convert to single value
    setCustomContext(key, newValue[0]);
    return {
      success: true,
      message: `Removed ${value} from ${key} (now: ${String(newValue[0])})`,
    };
  } else {
    // Array still has multiple elements
    setCustomContext(key, newValue);
    return {
      success: true,
      message: `Removed ${value} from ${key} (now: [${newValue.join(', ')}])`,
    };
  }
}

/**
 * Check if a command is a context command
 */
export function isContextCommand(command: { type?: string; name?: string }): boolean {
  return command.type === 'ctx' || command.name === 'ctx';
}
