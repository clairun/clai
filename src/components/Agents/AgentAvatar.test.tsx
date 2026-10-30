import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/react';
import AgentAvatar from './AgentAvatar';
import { MAIN_AVATAR_HUE, avatarRefFor, identityFor, mainIdentity } from './agentIdentity';
import { agentAvatar } from './avatarGenerator';

/** The generator's markup as the DOM serialises it. */
const asHtml = (svg: string) => {
  const div = document.createElement('div');
  div.innerHTML = svg;
  return div.innerHTML;
};

// Built the way call sites build it: a fresh object per render.
const reviewer = () =>
  identityFor({ id: 'ws-agent', agentDefinitionId: 'def-1', avatar: avatarRefFor('Code Reviewer') });

afterEach(() => {
  document.documentElement.removeAttribute('data-theme');
  vi.restoreAllMocks();
});

describe('AgentAvatar', () => {
  it('renders the generated SVG for the identity at the requested size', () => {
    const { container } = render(<AgentAvatar identity={reviewer()} size={20} />);
    const svg = container.querySelector('svg');
    expect(svg).not.toBeNull();
    expect(svg!.getAttribute('width')).toBe('20');
    expect(container.firstElementChild).toHaveStyle({ '--agent-avatar-size': '20px' });
  });

  it('is decorative unless a label is given', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} />);
    expect(container.firstElementChild).toHaveAttribute('aria-hidden', 'true');
    rerender(<AgentAvatar identity={reviewer()} label="Code Reviewer" />);
    expect(container.firstElementChild).toHaveAttribute('role', 'img');
    expect(container.firstElementChild).toHaveAttribute('aria-label', 'Code Reviewer');
  });

  it('draws a ring for idle/running only; disabled just dims', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} />);
    const el = () => container.firstElementChild as HTMLElement;
    expect(el().dataset.activity).toBe('none');
    expect(el().className).not.toMatch(/ring/);
    rerender(<AgentAvatar identity={reviewer()} activity="idle" />);
    expect(el().className).toMatch(/ring/);
    rerender(<AgentAvatar identity={reviewer()} activity="running" />);
    expect(el().className).toMatch(/ring/);
    expect(el().className).toMatch(/running/);
    rerender(<AgentAvatar identity={reviewer()} activity="disabled" />);
    expect(el().className).toMatch(/disabled/);
    expect(el().className).not.toMatch(/ring/);
  });

  it('ring={false} keeps the activity expression but draws no ring', () => {
    // The chat's in-flight footer needs the running face without the ring,
    // because the ring spins and that footer may not animate.
    const { container, rerender } = render(
      <AgentAvatar identity={reviewer()} activity="running" />
    );
    const el = () => container.firstElementChild as HTMLElement;
    const face = () => container.querySelector('svg')!.outerHTML;
    const runningFace = face();
    expect(el().className).toMatch(/ring/);
    expect(el().className).toMatch(/running/);

    rerender(<AgentAvatar identity={reviewer()} activity="running" ring={false} />);
    expect(el().className).not.toMatch(/ring/);
    expect(el().className).not.toMatch(/running/);
    // Same activity, same drawn face — only the ring is gone.
    expect(el().dataset.activity).toBe('running');
    expect(face()).toBe(runningFace);

    // `disabled` never had a ring to drop, and keeps its dimming either way.
    rerender(<AgentAvatar identity={reviewer()} activity="disabled" ring={false} />);
    expect(el().className).toMatch(/disabled/);
  });

  it('changes expression with activity but keeps the same body', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} />);
    const neutral = container.innerHTML;
    rerender(<AgentAvatar identity={reviewer()} activity="running" />);
    expect(container.innerHTML).not.toBe(neutral);
    // Same clip id ⇒ same seed drawn.
    const clip = (html: string) => /clipPath id="([^"]+)"/.exec(html)?.[1];
    expect(clip(container.innerHTML)).toBe(clip(neutral));
  });

  it('follows the app theme attribute on <html>', () => {
    document.documentElement.setAttribute('data-theme', 'dark');
    const { container, unmount } = render(<AgentAvatar identity={reviewer()} />);
    const dark = container.innerHTML;
    unmount();
    document.documentElement.setAttribute('data-theme', 'light');
    const { container: light } = render(<AgentAvatar identity={reviewer()} />);
    expect(light.innerHTML).not.toBe(dark);
  });

  it('exposes the identity hue as the ring colour variable', () => {
    const { container } = render(<AgentAvatar identity={mainIdentity()} activity="idle" />);
    const style = (container.firstElementChild as HTMLElement).style;
    expect(style.getPropertyValue('--agent-ring-color')).toMatch(/^#[0-9a-f]{6}$/);
  });

  it('a fresh identity object with the same fields renders identically', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} activity="idle" />);
    const first = container.innerHTML;
    rerender(<AgentAvatar identity={reviewer()} activity="idle" />);
    expect(container.innerHTML).toBe(first);
  });

  it("draws each workspace's Main from its own seed, in purple, with the accessory the seed picks", () => {
    const main = identityFor({ id: 'ws-1', isDefault: true });
    const a = render(<AgentAvatar identity={main} />).container.innerHTML;
    const b = render(<AgentAvatar identity={identityFor({ id: 'ws-2', isDefault: true })} />).container.innerHTML;
    expect(a).not.toBe(b);
    expect(a).toContain(asHtml(agentAvatar('ws-1', { size: 32, theme: 'light', hue: MAIN_AVATAR_HUE, accessory: true })));
    // The accessory is what the Main adds over the plain face.
    expect(agentAvatar('ws-1', { size: 32, theme: 'light', hue: MAIN_AVATAR_HUE, accessory: true })).not.toBe(
      agentAvatar('ws-1', { size: 32, hue: MAIN_AVATAR_HUE }),
    );
  });

  it('draws no accessory for a member', () => {
    const { container } = render(<AgentAvatar identity={reviewer()} />);
    expect(container.innerHTML).toContain(asHtml(agentAvatar('Code Reviewer', { size: 32, theme: 'light' })));
    expect(container.innerHTML).not.toContain(asHtml(agentAvatar('Code Reviewer', { size: 32, theme: 'light', accessory: true })));
  });
});
