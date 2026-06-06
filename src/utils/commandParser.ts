/**
 * Command parser for the CLAI terminal interface.
 *
 * Lightweight command parser that converts command strings into structured objects.
 * Philosophy: parse, don't validate. Let the registry handle command existence.
 */

import { COMMAND_STATUS } from './commandTypes';
import type { CommandStatus } from './commandTypes';

export type CommandOptionValue = string | boolean;

export interface CommandArgs {
  options: Record<string, CommandOptionValue>;
  positional: string[];
}

export interface ParsedCommand {
  id: string;
  type: string;
  raw: string;
  name: string;
  args: CommandArgs;
  timestamp: number;
  status: CommandStatus;
  error?: string;
}

/**
 * Generate a unique command ID
 */
const generateCommandId = (): string => {
  return `cmd_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
};

/**
 * Parse command line arguments into key-value pairs
 * Supports both --flag and --key=value formats
 */
export const parseArguments = (args: string[]): CommandArgs => {
  const options: Record<string, CommandOptionValue> = {};
  const positional: string[] = [];

  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!; // bounded by the loop condition

    // Handle --key=value format
    if (arg.startsWith('--') && arg.includes('=')) {
      const [key = '', ...valueParts] = arg.slice(2).split('=');
      options[key] = valueParts.join('=');
    }
    // Handle --flag or --key value format
    else if (arg.startsWith('--')) {
      const key = arg.slice(2);
      // Check if next arg is a value (doesn't start with --)
      if (i + 1 < args.length && !args[i + 1]!.startsWith('--')) {
        options[key] = args[i + 1]!;
        i++; // Skip next arg since we consumed it
      } else {
        // It's a boolean flag
        options[key] = true;
      }
    }
    // Handle -f format (short flags)
    else if (arg.startsWith('-') && arg.length === 2) {
      const key = arg.slice(1);
      // Check if next arg is a value
      if (i + 1 < args.length && !args[i + 1]!.startsWith('-')) {
        options[key] = args[i + 1]!;
        i++;
      } else {
        options[key] = true;
      }
    }
    // Positional argument
    else {
      positional.push(arg);
    }
  }

  return { options, positional };
};

/**
 * Parse a command string into a structured command object
 *
 * @example
 * parseCommand('metrics --range 1h')
 * // {
 * //   id: 'cmd_1234567890_abc123', type: 'metrics', raw: 'metrics --range 1h',
 * //   name: 'metrics', args: { options: { range: '1h' }, positional: [] },
 * //   timestamp: 1234567890, status: 'pending'
 * // }
 */
export const parseCommand = (commandString: string): ParsedCommand => {
  // Trim and handle empty commands
  const trimmed = commandString.trim();
  if (!trimmed) {
    return {
      id: generateCommandId(),
      type: '',
      raw: commandString,
      name: '',
      args: { options: {}, positional: [] },
      timestamp: Date.now(),
      status: COMMAND_STATUS.PENDING,
      error: 'Empty command',
    };
  }

  // Split command into parts, respecting quoted strings
  const parts = trimmed.match(/(?:[^\s"']+|"[^"]*"|'[^']*')+/g) || [];

  // First part is the command name (normalized to lowercase)
  const commandName = (parts[0] || '').toLowerCase().trim();

  // Rest are arguments
  const argParts = parts.slice(1);
  const args = parseArguments(argParts);

  return {
    id: generateCommandId(),
    type: commandName, // Type is just the command name
    raw: commandString,
    name: commandName,
    args,
    timestamp: Date.now(),
    status: COMMAND_STATUS.PENDING,
  };
};

