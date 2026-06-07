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
 *   - react-hooks/exhaustive-deps
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
 *
 * `react-hooks/preserve-manual-memoization` was enabled as a warning
 * on 2026-06-07 (clai#5). The rule validates that any manual
 * `useMemo`/`useCallback` is written in a way that React Compiler's
 * dependency inference can preserve — i.e. the inferred deps match
 * the manually specified deps array. When they don't match, the
 * compiler refuses to compile the component because preserving the
 * manual memoization could change render behavior (memo cache would
 * invalidate at different times than the user expects). The current
 * codebase is NOT clean — the rule surfaces 13 violations, all of
 * which are concentrated in a single file. Two distinct violation
 * shapes appear:
 *   - "This dependency may be modified later" — manual deps include
 *     a value (typically returned from a `useStore` selector or
 *     `useMemo` higher in the component) that the React Compiler
 *     flags as mutated after the memoization site. The compiler
 *     cannot guarantee the memoized closure won't see a stale value.
 *   - "This value was memoized in source but not in compilation
 *     output" — the manual `useCallback` body has logic the compiler
 *     can't model (e.g. function is conditionally not returned, or
 *     the function body contains a state-setter call that mutates a
 *     dep in an untracked scope), so the compiler drops the manual
 *     memoization entirely and reports the value as unmemoized.
 * All 13 affected lines live in:
 *   - src/components/ContextPanel/ContextPanel.tsx (lines 109, 128,
 *     141 [×4], 143 [×2], 152 [×4], 154, 168 [×4], with column
 *     offsets on multi-element deps arrays).
 * The fix is non-trivial: most call sites need to either pull the
 * captured value out of the deps array (if it's truly stable, e.g.
 * a setter or a non-mutated ref), or refactor the surrounding code
 * so the closure body doesn't reach into an untracked scope.
 * Cleanup will land in a focused follow-up PR; this PR is the
 * gate-establishment flip only. Once all 13 are resolved, this
 * rule will be promoted from `warn` to `error` in a separate PR.
 *
 * `react-hooks/set-state-in-effect` was enabled as a warning on
 * 2026-06-07 (clai#5). The rule validates that `useEffect` /
 * `useLayoutEffect` / `useInsertionEffect` callbacks do not call
 * a `setState` function synchronously in the effect body. The
 * rationale is that effects are intended to synchronize React with
 * external systems; calling a setState synchronously inside the
 * effect body causes a cascading render (React renders again as
 * soon as the effect commits) and is almost always a sign that
 * the data should have been derived during render, modeled as a
 * derived event, or read from the external system via
 * `useSyncExternalStore` instead. See
 * https://react.dev/learn/you-might-not-need-an-effect. The current
 * codebase is NOT clean — the rule surfaces 29 violations across
 * 14 files. Three distinct violation shapes appear, in order of
 * frequency:
 *   1. "Sync state from a prop on prop change" — the most common
 *      pattern, with 10+ instances. Example:
 *      src/components/Settings/WorkspaceSettingsModal.tsx:356
 *      runs `setSelection(initialSel)`, `setVisited(...)`,
 *      `setDirty({})`, `setSaving(false)`, `setSaveError(null)`
 *      whenever `[isOpen, initialSel]` changes. The React Compiler
 *      flags this as a derived-state pattern: `selection` could be
 *      computed from `initialSel` during render with a `key` or by
 *      lifting the state up, instead of being copied on every
 *      effect run.
 *   2. "Reset state when a key changes" — also 10+ instances, in
 *      files like `src/pages/Workspace.tsx:656` (resets
 *      `expanded` / `childrenByPath` whenever `workspaceId`
 *      changes), `src/contexts/TabContext.tsx:75` (resets
 *      `selectedMcpServerIds` etc. whenever `tabId` or
 *      `initialContext` changes), and `src/components/
 *      InlineApprovalCard.tsx:82` (clears `requests` on workspace
 *      switch). The fix is to either pass `key={workspaceId}` to
 *      remount the component, or to derive the per-key state from
 *      the key prop directly instead of mirroring it into
 *      separate useState slots.
 *   3. "Recompute derived data inside an effect" — the rarer
 *      pattern, e.g. `src/components/common/VirtualizedList.tsx:325`
 *      bumps a `measurementVersion` counter after pruning
 *      `heightsRef`. The fix is usually to fold the version bump
 *      into the data model (render the version as
 *      `heightsRef.size` or a hash of the items list) so no
 *      effect-driven setState is needed.
 * Affected files (counts in parens):
 *   - src/components/Settings/WorkspaceSettingsModal.tsx (7)
 *   - src/pages/Workspace.tsx (6)
 *   - src/components/WorkspaceFilePreviewPanel.tsx (3)
 *   - src/components/InlineApprovalCard.tsx (2)
 *   - src/components/InlinePathGrantCard.tsx (2)
 *   - src/components/Settings/AssistantProviderSettings.tsx (2)
 *   - src/components/Settings/McpServerFormModal.tsx (1)
 *   - src/components/Settings/SettingsModal.tsx (1)
 *   - src/components/WorkspaceTaskTranscriptPanel.tsx (1)
 *   - src/components/common/VirtualizedList.tsx (1)
 *   - src/contexts/TabContext.tsx (1)
 *   - src/contexts/TabManagerContext.tsx (1)
 *   - src/layouts/FleetLayout.tsx (1)
 * Cleanup will land in focused follow-up PRs (likely one PR per
 * file or one PR per pattern class); this PR is the
 * gate-establishment flip only. Once all 29 are resolved, this
 * rule will be promoted from `warn` to `error` in a separate PR.
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
      'react-hooks/set-state-in-effect': 'warn',
      'react-hooks/exhaustive-deps': 'off',
      'react-hooks/preserve-manual-memoization': 'warn',
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
      'react-hooks/set-state-in-effect': 'warn',
      'react-hooks/exhaustive-deps': 'off',
      'react-hooks/preserve-manual-memoization': 'warn',
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
