/**
 * The trigger an automation's run persists as a user message
 * (`turn_common.rs`): a boundary for the model, never shown to the user.
 */
export const isRunMarkerText = (text: string): boolean =>
  text.startsWith('--- New scheduled run at') || text.startsWith('--- Manual run at');
