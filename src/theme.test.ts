import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { initTheme, setThemePreference } from './theme';

beforeEach(() => {
  const items = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => { items.set(key, value); },
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  document.documentElement.removeAttribute('data-theme');
});

describe('theme synchronization', () => {
  it('applies a theme changed in another window through storage', () => {
    localStorage.setItem('clai.theme', 'light');
    initTheme();
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    localStorage.setItem('clai.theme', 'dark');
    window.dispatchEvent(new StorageEvent('storage', { key: 'clai.theme' }));
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    setThemePreference('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });
});
