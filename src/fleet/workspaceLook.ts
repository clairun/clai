/**
 * What tells one workspace's card from another's at a glance, derived from
 * its id alone, so it never changes and needs no setting: the accessory its
 * Main wears, an accent hue for its "Now" caption, and a screentone behind
 * the card. Main's face itself is the same in every workspace.
 */

/** Drawn over Main's face (`FleetFace`); Main already has an antenna of its own. */
export const ACCESSORIES = ['headband', 'glasses', 'cap', 'bow', 'scarf', 'earphones', 'scar', 'flower'] as const;
export type Accessory = (typeof ACCESSORIES)[number];

/** Drawn in the effects layer (`FleetEffects.module.css`). */
export const SCREENTONES = ['dots', 'lines', 'hatch', 'fade'] as const;
export type Screentone = (typeof SCREENTONES)[number];

/** OKLCH hues of the avatar palette, without red, amber and green: those say failed, needs you and done. */
export const WORKSPACE_HUES = [110, 170, 200, 230, 260, 290, 320, 350] as const;

export interface WorkspaceLook {
  accessory: Accessory;
  hue: number;
  screentone: Screentone;
}

/** FNV-1a, finished with murmur3's avalanche so every byte of the result depends on every character. */
const hash = (text: string): number => {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  return (h ^ (h >>> 16)) >>> 0;
};

/** One byte of the hash per trait, so the traits vary independently. */
export const workspaceLook = (workspaceId: string): WorkspaceLook => {
  const h = hash(workspaceId);
  return {
    accessory: ACCESSORIES[(h & 0xff) % ACCESSORIES.length]!,
    hue: WORKSPACE_HUES[((h >>> 8) & 0xff) % WORKSPACE_HUES.length]!,
    screentone: SCREENTONES[((h >>> 16) & 0xff) % SCREENTONES.length]!,
  };
};
