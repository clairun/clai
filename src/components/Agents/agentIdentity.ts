/**
 * Who an agent *looks like*, resolved from what we know about it.
 *
 * The picked face lives on the agent definition as `avatar: { seed,
 * generatorVersion }`. Agents saved before faces existed have none, so the
 * seed falls back to the definition id and then to the workspace-local id:
 * every agent has a stable face on first launch, no migration.
 *
 * The Main is the exception on purpose: it always wears the same face, in the
 * app's primary indigo, in every workspace. One face users learn once means
 * "the main of this workspace" wherever it shows up.
 */
import type { AgentAvatarRef } from '../../generated/bindings';
import {
  GENERATOR_VERSION,
  HUES,
  agentAvatar,
  hueIndexFor,
  paletteForHue,
  type AvatarMood,
  type AvatarOptions,
  type AvatarTheme,
} from './avatarGenerator';

export { GENERATOR_VERSION };

/** Fixed seed for every workspace's Main. Chosen by hand from a candidate sheet. */
export const MAIN_AVATAR_SEED = 'clai-main-847';
/** OKLCH hue of `--color-primary` (#818CF8). The Main ignores its seed's hue. */
export const MAIN_AVATAR_HUE = 277;

export interface AgentIdentity {
  seed: string;
  /** `undefined` means "use the hue the seed maps to". */
  hue?: number;
  generatorVersion: number;
}

/** The subset of `WorkspaceAgentResponse` / `AgentDefinitionDetail` identity needs. */
export interface AgentLike {
  id?: string | null;
  agentDefinitionId?: string | null;
  isDefault?: boolean | null;
  avatar?: AgentAvatarRef | null;
}

export const mainIdentity = (): AgentIdentity => ({
  seed: MAIN_AVATAR_SEED,
  hue: MAIN_AVATAR_HUE,
  generatorVersion: GENERATOR_VERSION,
});

/** Resolve an agent's face: Main → fixed; else picked face → definition id → local id. */
export const identityFor = (agent: AgentLike): AgentIdentity => {
  if (agent.isDefault) return mainIdentity();
  if (agent.avatar?.seed) {
    return { seed: agent.avatar.seed, generatorVersion: agent.avatar.generatorVersion };
  }
  return {
    seed: agent.agentDefinitionId || agent.id || '',
    generatorVersion: GENERATOR_VERSION,
  };
};

/** What the create flow saves once a candidate is picked. */
export const avatarRefFor = (seed: string): AgentAvatarRef => ({
  seed,
  generatorVersion: GENERATOR_VERSION,
});

/** OKLCH hue in degrees for an identity, for rings and hairlines drawn by CSS. */
export const identityHue = (identity: AgentIdentity): number =>
  identity.hue ?? (HUES[hueIndexFor(identity.seed)] as number);

export const identityRingColor = (identity: AgentIdentity, theme: AvatarTheme): string =>
  paletteForHue(identityHue(identity), theme).ring;

export const renderIdentity = (
  identity: AgentIdentity,
  options: Omit<AvatarOptions, 'hue' | 'version'> = {},
): string =>
  agentAvatar(identity.seed, {
    ...options,
    hue: identity.hue,
    version: identity.generatorVersion,
  });

/**
 * Seeds for the "pick a face" row shown when creating or editing an agent.
 * Every candidate gets a distinct hue so the row reads as six different
 * agents, not six recolours of one. Deterministic for a given nonce so tests
 * and the Shuffle button (new nonce each press) behave predictably.
 */
export const candidateSeeds = (nonce: string, count = 6): string[] => {
  const seeds: string[] = [];
  const seenHues = new Set<number>();
  const max = Math.min(count, HUES.length);
  for (let i = 0; seeds.length < max && i < 1000; i++) {
    const seed = `${nonce}-${i}`;
    const hue = hueIndexFor(seed);
    if (seenHues.has(hue)) continue;
    seenHues.add(hue);
    seeds.push(seed);
  }
  return seeds;
};

/** A fresh, unguessable nonce for `candidateSeeds`. */
export const freshNonce = (): string =>
  typeof crypto !== 'undefined' && 'randomUUID' in crypto
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

export type AgentActivity = 'idle' | 'running' | 'attention' | 'disabled' | 'none';

/** Map what an agent is doing to the expression the generator draws. */
export const moodFor = (activity: AgentActivity | undefined): AvatarMood => {
  switch (activity) {
    case 'running':
      return 'running';
    case 'attention':
      return 'attention';
    case 'idle':
    case 'disabled':
      return 'idle';
    default:
      return 'neutral';
  }
};
