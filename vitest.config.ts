import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['ts/tests/**/*.test.ts', 'ts/tests/**/*.spec.ts'],
    /**
     * e2e tests boot bankrun (~3 seconds) and run sequential ixs against the
     * in-process Solana runtime. They share `beforeAll` state inside a spec
     * file, so each `*.spec.ts` must run start-to-finish without parallel
     * interleaving. Tests within a single file also share state — keep them
     * in declaration order.
     */
    testTimeout: 60_000,
    hookTimeout: 60_000,
    pool: 'forks',
    poolOptions: {
      forks: { singleFork: false },
    },
  },
});
