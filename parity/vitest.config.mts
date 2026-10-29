import { defineConfig } from 'vitest/config';

export default defineConfig({
  root: '.work',
  test: {
    globals: true,
    environment: 'node',
    include: ['__tests__/**/*.test.ts'],
    // Each test indexes a fresh temp project through the binary.
    testTimeout: 60_000,
    env: { CODEGRAPH_TELEMETRY: '0', DO_NOT_TRACK: '1' },
  },
});
