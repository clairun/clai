import js from '@eslint/js';
import globals from 'globals';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import tseslint from 'typescript-eslint';
import prettier from 'eslint-config-prettier';

/**
 * ESLint flat config for CLAI frontend.
 *
 * React 19 strict hooks rules (all 7 sub-rules of
 * eslint-plugin-react-hooks v7) are enforced as ERRORS as of
 * 2026-06-12. They were introduced as warnings between 2026-06-05
 * and 2026-06-07 (clai#5), the violations were cleaned up across
 * focused PRs (see clai#5 for the per-rule inventory and the sub-PR
 * series), and the rules were promoted to `error` once the codebase
 * reached zero violations. The handful of deliberate exceptions are
 * suppressed inline with `eslint-disable-next-line` comments that
 * carry their own justification.
 *
 * Keep the two rule blocks (TS and JS) in sync when changing rules.
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
      'react-hooks/set-state-in-effect': 'error',
      'react-hooks/exhaustive-deps': 'error',
      'react-hooks/preserve-manual-memoization': 'error',
      'react-hooks/refs': 'error',
      'react-hooks/immutability': 'error',
      'react-hooks/use-memo': 'error',
      'react-hooks/purity': 'error',
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
      'react-hooks/set-state-in-effect': 'error',
      'react-hooks/exhaustive-deps': 'error',
      'react-hooks/preserve-manual-memoization': 'error',
      'react-hooks/refs': 'error',
      'react-hooks/immutability': 'error',
      'react-hooks/use-memo': 'error',
      'react-hooks/purity': 'error',
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
