/**
 * What tells one workspace's card from another's at a glance, derived from
 * its id alone, so it never changes and needs no setting: an accent hue for
 * its "Now" caption.
 */

/** OKLCH hues of the avatar palette, without red, amber and green: those say failed, needs you and done. */
export const WORKSPACE_HUES = [110, 170, 200, 230, 260, 290, 320, 350] as const;

export interface WorkspaceLook {
  hue: number;
}

/** FNV-1a, finished with murmur3's avalanche so every byte of the result depends on every character. */
const hash = (text: string): number => {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  return (h ^ (h >>> 16)) >>> 0;
};

/** The hue takes the hash's second byte, as it always has, so no workspace changes colour. */
export const workspaceLook = (workspaceId: string): WorkspaceLook => ({
  hue: WORKSPACE_HUES[((hash(workspaceId) >>> 8) & 0xff) % WORKSPACE_HUES.length]!,
});
