/**
 * Command Types for CLAI terminal commands
 *
 * Only the command-record status survives: the layout (/tab, /tile,
 * /reset-all), system (/ctx) and content (visualization) command families
 * were removed with the legacy tabs/tiles UI. CommandContext still records
 * command history entries with these statuses.
 */

// Command Status
export const COMMAND_STATUS = {
  PENDING: 'pending',
  EXECUTING: 'executing',
  SUCCESS: 'success',
  ERROR: 'error',
  CANCELLED: 'cancelled',
} as const;

export type CommandStatus = (typeof COMMAND_STATUS)[keyof typeof COMMAND_STATUS];
