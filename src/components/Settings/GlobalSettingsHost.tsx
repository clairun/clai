import { useEffect, useState } from 'react';
import SettingsModal, { TABS } from './SettingsModal';
import { OPEN_GLOBAL_SETTINGS_EVENT, type OpenGlobalSettingsDetail } from '../../utils/globalSettings';

/**
 * The global Settings modal, hosted once in MainLayout so every page (the
 * Fleet included) can open it through `openGlobalSettings()`.
 */
const GlobalSettingsHost = () => {
  // `null` while closed. The deep-link fields (provider form, agent editor)
  // are dropped on close, so a later plain open doesn't replay them.
  const [open, setOpen] = useState<OpenGlobalSettingsDetail | null>(null);

  useEffect(() => {
    const handleOpen = (event: Event) => setOpen((event as CustomEvent<OpenGlobalSettingsDetail>).detail || {});
    window.addEventListener(OPEN_GLOBAL_SETTINGS_EVENT, handleOpen);
    return () => window.removeEventListener(OPEN_GLOBAL_SETTINGS_EVENT, handleOpen);
  }, []);

  return (
    <SettingsModal
      isOpen={open != null}
      onClose={() => setOpen(null)}
      initialTab={open?.tab ?? TABS.PROVIDER}
      initialProviderAction={open?.providerAction ?? null}
      initialAgentDefinitionId={open?.agentDefinitionId ?? null}
    />
  );
};

export default GlobalSettingsHost;
