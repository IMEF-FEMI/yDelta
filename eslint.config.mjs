// ESLint v9 flat config. Replaces the legacy `.eslintrc`.
//
// Targets only `ts/**/*.ts` — Rust + Anchor build outputs + node_modules
// are explicitly excluded. The `typescript-eslint` umbrella package
// pulls in both the parser and the plugin in a single import.
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import prettier from 'eslint-config-prettier';

export default tseslint.config(
  {
    // Global ignores — keep at the top so they apply across every config
    // block below.
    ignores: ['dist/', 'node_modules/', '.yarn/', 'target/', 'lib/', 'ts/tests/**'],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  prettier,
  {
    files: ['ts/**/*.ts'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: {
        // Node globals — the action scripts touch fs / process / fetch.
        process: 'readonly',
        console: 'readonly',
        Buffer: 'readonly',
        URL: 'readonly',
        fetch: 'readonly',
        // Node 18+ stable global; flagged by `no-undef` otherwise.
        AbortController: 'readonly',
      },
    },
    rules: {
      'linebreak-style': ['error', 'unix'],
      semi: ['error', 'always'],
      '@typescript-eslint/no-non-null-assertion': 'off',
      '@typescript-eslint/ban-ts-comment': 'off',
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-non-null-asserted-optional-chain': 'off',
      '@typescript-eslint/explicit-function-return-type': 'warn',
      '@typescript-eslint/no-unused-vars': [
        'error',
        {
          argsIgnorePattern: '^_',
          varsIgnorePattern: '^_',
          caughtErrorsIgnorePattern: '^_',
        },
      ],
      // The scripts use BigInt literals + dynamic imports; default off for
      // those rules under the umbrella plugin already, but pin here so a
      // future preset bump doesn't silently re-enable them.
      '@typescript-eslint/no-require-imports': 'off',
    },
  },
);
