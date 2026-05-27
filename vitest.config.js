import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// Separate config from vite.config.js because the prod build uses
// `vite-plugin-singlefile` which doesn't play with vitest's module graph
// and is irrelevant for tests anyway.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.{test,spec}.{js,jsx,ts,tsx}'],
    css: false,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'html', 'json-summary'],
      reportsDirectory: './coverage',
      include: ['src/**/*.{ts,tsx}'],
      // Exclude generated bindings, test files/harness, and barrel/index
      // re-exports that carry no logic.
      exclude: [
        'src/generated/**',
        'src/test/**',
        'src/**/*.{test,spec}.{ts,tsx}',
        'src/**/index.ts',
        'src/main.tsx',
      ],
      // No failing thresholds yet — the suite is young (5 files). Coverage is
      // reported so we can ratchet a gate up (assistant/ first) as the suite
      // grows. See docs/TESTING_AND_TYPES.md P2-3.
    },
  },
});
