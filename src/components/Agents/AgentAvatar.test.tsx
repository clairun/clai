import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/react';
import AgentAvatar from './AgentAvatar';
import { avatarRefFor, identityFor, mainIdentity } from './agentIdentity';

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

  it('draws a ring for idle/running/attention only; disabled just dims', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} />);
    const el = () => container.firstElementChild as HTMLElement;
    expect(el().dataset.activity).toBe('none');
    expect(el().className).not.toMatch(/ring/);
    rerender(<AgentAvatar identity={reviewer()} activity="idle" />);
    expect(el().className).toMatch(/ring/);
    rerender(<AgentAvatar identity={reviewer()} activity="running" />);
    expect(el().className).toMatch(/ring/);
    expect(el().className).toMatch(/running/);
    rerender(<AgentAvatar identity={reviewer()} activity="attention" />);
    expect(el().className).toMatch(/ring/);
    expect(el().className).toMatch(/attention/);
    rerender(<AgentAvatar identity={reviewer()} activity="disabled" />);
    expect(el().className).toMatch(/disabled/);
    expect(el().className).not.toMatch(/ring/);
  });

  it('changes expression with activity but keeps the same body', () => {
    const { container, rerender } = render(<AgentAvatar identity={reviewer()} />);
    const neutral = container.innerHTML;
    rerender(<AgentAvatar identity={reviewer()} activity="attention" />);
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

  it('the Main gets its fixed indigo face regardless of the agent record', () => {
    const a = render(<AgentAvatar identity={identityFor({ id: 'ws-1', isDefault: true })} />);
    const b = render(
      <AgentAvatar identity={identityFor({ id: 'ws-2', avatar: avatarRefFor('other'), isDefault: true })} />,
    );
    expect(a.container.innerHTML).toBe(b.container.innerHTML);
    expect(a.container.innerHTML).toBe(
      render(<AgentAvatar identity={mainIdentity()} />).container.innerHTML,
    );
  });
});
