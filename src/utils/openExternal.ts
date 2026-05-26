/**
 * Opens an external URL in the system browser.
 *
 * Focus caveat: on Wayland (and on macOS/Windows when the browser is
 * already running) the OS may open the URL in an existing browser
 * instance without raising its window — `xdg-open` and friends don't
 * carry a compositor-issued activation token, and self-minted tokens
 * are ignored by strict Wayland compositors. The reliable cross-
 * platform fix is `org.freedesktop.portal.OpenURI` over D-Bus, which
 * we don't wire up here. Accepting the limitation keeps this path a
 * one-liner.
 *
 * @param url - The URL to open
 */
export async function openExternal(url: string): Promise<void> {
  try {
    const { openUrl } = await import('@tauri-apps/plugin-opener');
    await openUrl(url);
  } catch {
    window.open(url, '_blank');
  }
}
