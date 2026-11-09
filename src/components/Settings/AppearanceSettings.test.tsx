import { act, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import AppearanceSettings from './AppearanceSettings';

const items = new Map<string, string>();

afterEach(() => {
  vi.unstubAllGlobals();
  items.clear();
});

describe('AppearanceSettings cross-window updates', () => {
  it('updates the selected radio when another window changes the theme', () => {
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => { items.set(key, value); },
    });
    items.set('clai.theme', 'light');
    render(<AppearanceSettings />);
    expect(screen.getByRole('radio', { name: /Light/ })).toHaveAttribute('aria-checked', 'true');

    items.set('clai.theme', 'dark');
    act(() => window.dispatchEvent(new StorageEvent('storage', { key: 'clai.theme' })));
    expect(screen.getByRole('radio', { name: /Dark/ })).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByRole('radio', { name: /Light/ })).toHaveAttribute('aria-checked', 'false');
  });
});
