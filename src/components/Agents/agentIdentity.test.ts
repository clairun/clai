import { describe, expect, it } from 'vitest';
import {
  GENERATOR_VERSION,
  MAIN_AVATAR_HUE,
  MAIN_AVATAR_SEED,
  avatarRefFor,
  candidateSeeds,
  freshNonce,
  identityFor,
  identityHue,
  identityRingColor,
  mainIdentity,
  moodFor,
  renderIdentity,
} from './agentIdentity';
import { HUES, agentAvatar, hueIndexFor } from './avatarGenerator';

describe('identityFor', () => {
  it('gives every Main the same fixed face regardless of ids or saved seed', () => {
    const a = identityFor({ id: 'ws-1-main', agentDefinitionId: 'def-1', isDefault: true });
    const b = identityFor({
      id: 'ws-2-main',
      avatar: { seed: 'custom', generatorVersion: 1 },
      isDefault: true,
    });
    expect(a).toEqual(mainIdentity());
    expect(b).toEqual(mainIdentity());
    expect(a.seed).toBe(MAIN_AVATAR_SEED);
    expect(a.hue).toBe(MAIN_AVATAR_HUE);
  });

  it('prefers the picked face, then the definition id, then the local id', () => {
    const picked = { seed: 's', generatorVersion: 1 };
    expect(identityFor({ id: 'l', agentDefinitionId: 'd', avatar: picked }).seed).toBe('s');
    expect(identityFor({ id: 'l', agentDefinitionId: 'd', avatar: null }).seed).toBe('d');
    expect(identityFor({ id: 'l', agentDefinitionId: 'd' }).seed).toBe('d');
    expect(identityFor({ id: 'l' }).seed).toBe('l');
    expect(identityFor({}).seed).toBe('');
  });

  it('keeps the picked generator version; derived faces use the current one', () => {
    expect(identityFor({ id: 'l', avatar: { seed: 's', generatorVersion: 7 } }).generatorVersion).toBe(7);
    expect(identityFor({ id: 'l' }).generatorVersion).toBe(GENERATOR_VERSION);
    expect(identityFor({ id: 'l', avatar: null }).generatorVersion).toBe(GENERATOR_VERSION);
  });

  it('a non-Main identity never carries a hue override', () => {
    expect(identityFor({ id: 'l', avatar: { seed: 's', generatorVersion: 1 } }).hue).toBeUndefined();
  });

  it('avatarRefFor stamps the current generator version on a picked seed', () => {
    expect(avatarRefFor('nonce-3')).toEqual({ seed: 'nonce-3', generatorVersion: GENERATOR_VERSION });
    expect(identityFor({ id: 'l', avatar: avatarRefFor('nonce-3') }).seed).toBe('nonce-3');
  });
});

describe('identityHue / identityRingColor / renderIdentity', () => {
  it('hue follows the seed unless overridden', () => {
    const id = identityFor({ id: 'l', avatar: avatarRefFor('Code Reviewer') });
    expect(identityHue(id)).toBe(HUES[hueIndexFor('Code Reviewer')]);
    expect(identityHue(mainIdentity())).toBe(MAIN_AVATAR_HUE);
  });

  it('ring colour is a hex colour and differs between themes', () => {
    const id = identityFor({ id: 'l', avatar: avatarRefFor('Code Reviewer') });
    expect(identityRingColor(id, 'dark')).toMatch(/^#[0-9a-f]{6}$/);
    expect(identityRingColor(id, 'dark')).not.toBe(identityRingColor(id, 'light'));
  });

  it('renderIdentity forwards seed, hue and version to the generator', () => {
    const id = identityFor({ id: 'l', avatar: avatarRefFor('Code Reviewer') });
    expect(renderIdentity(id, { size: 20 })).toBe(agentAvatar('Code Reviewer', { size: 20 }));
    expect(renderIdentity(mainIdentity())).toBe(
      agentAvatar(MAIN_AVATAR_SEED, { hue: MAIN_AVATAR_HUE }),
    );
  });
});

describe('candidateSeeds', () => {
  it('returns the requested count with pairwise-distinct hues, deterministically', () => {
    const seeds = candidateSeeds('nonce-a');
    expect(seeds).toHaveLength(6);
    expect(new Set(seeds.map(hueIndexFor)).size).toBe(6);
    expect(candidateSeeds('nonce-a')).toEqual(seeds);
    expect(candidateSeeds('nonce-b')).not.toEqual(seeds);
  });

  it('caps at the number of hues', () => {
    expect(candidateSeeds('n', 40)).toHaveLength(HUES.length);
    expect(candidateSeeds('n', 0)).toHaveLength(0);
  });

  it('seeds are derived from the nonce so Shuffle gets a new row', () => {
    for (const seed of candidateSeeds('abc')) expect(seed.startsWith('abc-')).toBe(true);
  });
});

describe('freshNonce', () => {
  it('is non-empty and unique across calls', () => {
    const a = freshNonce();
    const b = freshNonce();
    expect(a.length).toBeGreaterThan(8);
    expect(a).not.toBe(b);
  });
});

describe('moodFor', () => {
  it('maps activity to expression', () => {
    expect(moodFor('running')).toBe('running');
    expect(moodFor('attention')).toBe('attention');
    expect(moodFor('idle')).toBe('idle');
    expect(moodFor('disabled')).toBe('idle');
    expect(moodFor('none')).toBe('neutral');
    expect(moodFor(undefined)).toBe('neutral');
  });
});
