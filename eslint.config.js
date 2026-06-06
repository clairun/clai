import js from '@eslint/js';
import globals from 'globals';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import tseslint from 'typescript-eslint';
import prettier from 'eslint-config-prettier';

/**
 * ESLint flat config for CLAI frontend.
 *
 * React 19 strict hooks rules are disabled for now because the
 * existing codebase was written before these strict rules existed.
 * Re-enable them incrementally in follow-up PRs:
 *   - react-hooks/set-state-in-effect
 *   - react-hooks/exhaustive-deps
 *   - react-hooks/preserve-manual-memoization
 *
 * `react-hooks/purity` was enabled as a warning on 2026-06-05 (clai#5).
 * The current codebase is clean for this rule (0 violations), so the
 * next step is to promote it from `warn` to `error` in a follow-up PR.
 *
 * `react-hooks/use-memo` was enabled as a warning on 2026-06-06 (clai#5).
 * The current codebase is clean for this rule (0 violations) — every
 * `useMemo`/`useCallback` call site already uses an inline arrow
 * function and a simple array-literal dep array. The next step for
 * this rule is also to promote it from `warn` to `error` in a
 * follow-up PR, after the `purity` promotion lands.
 *
 * `react-hooks/refs` was enabled as a warning on 2026-06-06 (clai#5).
 * Unlike the previous two sub-rules, the current codebase is NOT
 * clean for this rule — the rule surfaces 16 violations across
 * 7 files, all of which follow the "latest-value ref" anti-pattern
 * (syncing a prop or state into a ref directly during render, or
 * reading a ref cache to derive a value during render). Examples:
 *   - src/contexts/ChatManagerContext.tsx:54
 *   - src/assistant/useAssistantSession.ts:30
 *   - src/components/Chat/StreamingMarkdown.tsx:38,39
 *   - src/components/common/VirtualizedList.tsx:184,351
 *   - src/components/Settings/WorkspaceSettingsModal.tsx:620,1456,1457,1508
 * These are well-known patterns that the React Compiler explicitly
 * disallows because they bypass React's render model. The fix is
 * to move the sync into `useEffect`/`useLayoutEffect` or to refactor
 * to pass the value directly. That cleanup will land in focused
 * follow-up PRs; this PR is the gate-establishment flip only.
 * Once all 16 are resolved, this rule will be promoted from
 * `warn` to `error` in a separate PR.
 *
 * `react-hooks/immutability` was enabled as a warning on 2026-06-06
 * (clai#5). Like `refs`, the current codebase is NOT clean — the
 * rule surfaces 2 violations, both of which follow the same
 * "useEffect captures a function declared later in the same
 * component body" pattern (the function is declared with `const`
 * below the effect, but the effect calls it on mount). Affected sites:
 *   - src/components/Settings/McpServersSettings.tsx:53 — `useEffect`
 *     calls `loadServers()` declared at line 61.
 *   - src/components/Settings/SkillsSettings.tsx:88 — `useEffect`
 *     calls `loadCatalog()` declared at line 92.
 * The rule reports these as "Cannot access variable before it is
 * declared" because the closure created by `useEffect` references a
 * `const` binding that is in the temporal dead zone at the point of
 * effect creation. The fix is to declare the function above the
 * effect (or move the function body inline into the effect); both
 * are mechanical, no behavior change. Cleanup will land in a focused
 * follow-up PR; this PR is the gate-establishment flip only.
 */
export default [
  js.configs.recommended,
  ...tseslint.configs.recommended.map((config) => ({
    ...config,
    files: ['**/*.{ts,tsx}'],
  })),
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.es2021,
      },
    },
    plugins: {
      react,
      'react-hooks': reactHooks,
    },
    rules: {
      ...react.configs.recommended.rules,
      'react/react-in-jsx-scope': 'off',
      'react/prop-types': 'off',
      'react/display-name': 'off',
      'react/no-unescaped-entities': 'off',
      'no-unused-vars': 'off',
      '@typescript-eslint/no-unused-vars': ['warn', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
      'react-hooks/set-state-in-effect': 'off',
      'react-hooks/exhaustive-deps': 'off',
      'react-hooks/preserve-manual-memoization': 'off',
      'react-hooks/refs': 'warn',
      'react-hooks/immutability': 'warn',
      'react-hooks/use-memo': 'warn',
      'react-hooks/purity': 'warn',
    },
    settings: {
      react: { version: 'detect' },
    },
  },
  {
    files: ['**/*.{js,jsx}'],
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
      globals: {
        ...globals.browser,
        ...globals.es2021,
      },
      parserOptions: {
        ecmaFeatures: {
          jsx: true,
        },
      },
    },
    plugins: {
      react,
      'react-hooks': reactHooks,
    },
    rules: {
      ...react.configs.recommended.rules,
      'react/react-in-jsx-scope': 'off',
      'react/prop-types': 'off',
      'react/display-name': 'off',
      'react/no-unescaped-entities': 'off',
      'no-unused-vars': ['warn', { argsIgnorePattern: '^_' }],
      // Disabled React 19 strict hooks rules (see top comment)
      'react-hooks/set-state-in-effect': 'off',
      'react-hooks/exhaustive-deps': 'off',
      'react-hooks/preserve-manual-memoization': 'off',
      'react-hooks/refs': 'warn',
      'react-hooks/immutability': 'warn',
      'react-hooks/use-memo': 'warn',
      'react-hooks/purity': 'warn',
    },
    settings: {
      react: {
        version: 'detect',
      },
    },
  },
  {
    files: ['**/*.config.{js,mjs,cjs}'],
    languageOptions: {
      globals: globals.node,
    },
  },
  prettier,
];
