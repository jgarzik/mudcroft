import { test as base, expect, type Browser, type Page } from '@playwright/test'

/**
 * Shared fixtures and helpers for e2e tests.
 *
 * External font requests (index.html loads Google Fonts) are blocked so
 * tests never depend on the network.
 */
export const test = base.extend({
  context: async ({ context }, use) => {
    await context.route(/fonts\.(googleapis|gstatic)\.com/, (route) => route.abort())
    await use(context)
  },
})

export { expect }

let counter = 0

/** A username unique to this test run (tests share one server) */
export function uniqueName(prefix: string): string {
  counter += 1
  return `${prefix}_${process.pid}_${Date.now() % 100000}_${counter}`.slice(0, 32)
}

export const PASSWORD = 'e2e-player-pass'

/** Register a new account through the UI; ends on the universe picker */
export async function register(page: Page, username: string) {
  await page.goto('/')
  await page.getByTestId('auth-tab-register').click()
  await page.getByTestId('auth-username').fill(username)
  await page.getByTestId('auth-password').fill(PASSWORD)
  await page.getByTestId('auth-submit').click()
  await expect(page.getByTestId('universe-list')).toBeVisible()
}

/** Pick the seeded universe and wait until the game connection is up */
export async function enterWorld(page: Page) {
  await page.getByTestId('universe-item').filter({ hasText: 'E2E World' }).click()
  await page.getByTestId('enter-world').click()
  await expect(page.getByTestId('connection-status')).toHaveText('connected')
  await expect(page.getByTestId('room-name')).toHaveText('Cave Entrance')
}

/** Type a command into the terminal */
export async function command(page: Page, text: string) {
  const input = page.getByTestId('command-input')
  await input.fill(text)
  await input.press('Enter')
}

/** The terminal's message list */
export function terminal(page: Page) {
  return page.getByTestId('terminal-messages')
}

/** A fresh player in the world, in its own browser context */
export async function newPlayer(browser: Browser, prefix: string) {
  const context = await browser.newContext()
  await context.route(/fonts\.(googleapis|gstatic)\.com/, (route) => route.abort())
  const page = await context.newPage()
  const name = uniqueName(prefix)
  await register(page, name)
  await enterWorld(page)
  return { context, page, name }
}
