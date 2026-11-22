import { defineConfig, devices } from '@playwright/test'

/**
 * End-to-end tests: headless Chromium drives the built web client
 * (client/dist) served by a real mudd on a random port.
 *
 * e2e/global-setup.ts builds a throwaway server (temp dir, fresh database,
 * seeded universe) and exports its URL; e2e/global-teardown.ts stops it and
 * deletes the temp dir. Run via `npm run test:e2e` (after building mudd and
 * the client) or `scripts/e2e.py` from the repo root, which builds both.
 */
export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.ts',
  globalTeardown: './e2e/global-teardown.ts',
  // One shared server; tests isolate themselves with unique usernames
  fullyParallel: true,
  workers: 4,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: [['list'], ['html', { open: 'never', outputFolder: 'playwright-report' }]],
  outputDir: 'test-results',
  use: {
    baseURL: process.env.MUDCROFT_E2E_URL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
})
