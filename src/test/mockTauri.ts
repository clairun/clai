/**
 * Shared helpers for mocking Tauri in vitest.
 *
 * `vi.mock` is hoisted by vitest, so the actual mock declaration has
 * to live at the top of each `*.test.tsx` file. This module exports
 * the canonical pattern as a const for documentation, plus the
 * `getInvokeMock()` accessor that retrieves the mocked invoke handle
 * after the test file has installed it.
 *
 * Usage in a test file:
 *
 *   import { vi } from 'vitest';
 *   const mockInvoke = vi.hoisted(() => vi.fn());
 *   vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
 *
 *   // …in a test body:
 *   expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', { ... });
 */

export const MOCK_TAURI_PATTERN = `
import { vi } from 'vitest';
const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
` as const;
