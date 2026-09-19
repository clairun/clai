/**
 * Cross-tree opener for the global SettingsModal.
 *
 * The modal is hosted once in FleetLayout, but deep leaf components (e.g. the
 * chat context bars) need to open it at a specific section — and optionally
 * straight into a sub-action like the "Add Connection" form. Following the
 * existing cross-tree pattern (`assistant-provider-connections-changed`,
 * `mcp-servers-changed`), this is a window CustomEvent rather than threading
 * callbacks through every intermediate layer.
 */

export const OPEN_GLOBAL_SETTINGS_EVENT = 'open-global-settings';

export interface OpenGlobalSettingsDetail {
  /** Settings tab to open. Matches SettingsModal's TABS values. */
  tab?: 'provider' | 'agents' | 'skills' | 'mcp_servers' | 'applications' | 'appearance' | 'about';
  /** Open the provider tab with the "Add Connection" form already open. */
  providerAction?: 'new';
  /**
   * Open the agents tab with this shared definition already in its editor.
   * Ignored if the library no longer has it (deleted, archived, stale event):
   * the gallery is shown instead.
   */
  agentDefinitionId?: string;
}

export const openGlobalSettings = (detail: OpenGlobalSettingsDetail = {}): void => {
  window.dispatchEvent(new CustomEvent(OPEN_GLOBAL_SETTINGS_EVENT, { detail }));
};
