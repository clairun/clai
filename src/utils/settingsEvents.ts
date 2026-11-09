import { emit, listen } from '@tauri-apps/api/event';

export type SettingsEvent =
  | 'assistant-provider-connections-changed'
  | 'mcp-servers-changed'
  | 'workspace-settings-changed';

const events: SettingsEvent[] = [
  'assistant-provider-connections-changed',
  'mcp-servers-changed',
  'workspace-settings-changed',
];
const source = `${Date.now()}-${Math.random()}`;

export function notifySettingsChanged(event: SettingsEvent): void {
  window.dispatchEvent(new CustomEvent(event));
  void emit(event, source).catch((error) => console.error('Failed to broadcast settings change', error));
}

export function listenForSettingsChanges(): () => void {
  let active = true;
  const unlisteners: Array<() => void> = [];
  for (const event of events) {
    void listen<string>(event, (received) => {
      if (active && received.payload !== source) window.dispatchEvent(new CustomEvent(event));
    }).then((unlisten) => {
      if (active) unlisteners.push(unlisten);
      else unlisten();
    }).catch((error) => console.error('Failed to listen for settings changes', error));
  }
  return () => {
    active = false;
    unlisteners.forEach((unlisten) => unlisten());
  };
}
