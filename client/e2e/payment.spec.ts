import type { Page } from '@playwright/test'
import { test, expect, enterWorld, command, terminal, uniqueName, PASSWORD } from './fixtures'
import { ADMIN } from './server'

/** Log in through the UI; ends on the universe picker */
async function login(page: Page, username: string, password: string) {
  await page.goto('/')
  await page.getByTestId('auth-tab-login').click()
  await page.getByTestId('auth-username').fill(username)
  await page.getByTestId('auth-password').fill(password)
  await page.getByTestId('auth-submit').click()
  await expect(page.getByTestId('universe-list')).toBeVisible()
}

test('a payment is charged only after the player confirms it', async ({ page, browser, request }) => {
  const name = uniqueName('payer')
  const registered = await request.post('/auth/register', { data: { username: name, password: PASSWORD } })
  const { account_id } = await registered.json()

  // An admin builds a tip jar that asks for 3 credits, and funds the player
  const adminContext = await browser.newContext()
  const admin = await adminContext.newPage()
  await login(admin, ADMIN.username, ADMIN.password)
  await enterWorld(admin)
  // Object paths allow lowercase letters, digits and hyphens only
  const jar = `/items/jar-${name.replace(/_/g, '-')}`
  await command(
    admin,
    `eval local h = game.store_code([[return { tip = function(a) game.request_payment(3, "a tip", "tipped") return nil end, tipped = function(a) return "Thanks for the tip!" end }]]) ` +
      `game.create_object("${jar}", "item", "/rooms/cave-entrance", {name = "jar ${name}", fixed = true, code_hash = h}) ` +
      `game.add_action("tip", "${jar}", "tip") return "funded " .. tostring(game.admin_grant_credits("${account_id}", 10))`,
  )
  await expect(terminal(admin)).toContainText('funded true')
  await adminContext.close()

  await login(page, name, PASSWORD)
  await enterWorld(page)

  // Declining charges nothing
  await command(page, `tip jar ${name}`)
  await expect(page.getByTestId('payment-dialog')).toBeVisible()
  await expect(page.getByTestId('payment-text')).toContainText('Pay 3 credits for a tip?')
  await page.getByTestId('payment-decline').click()
  await expect(page.getByTestId('payment-dialog')).toBeHidden()
  await expect(terminal(page)).toContainText('You decline to pay 3 credits.')

  // Accepting pays, and the object's handler answers
  await command(page, `tip jar ${name}`)
  await page.getByTestId('payment-accept').click()
  await expect(page.getByTestId('payment-dialog')).toBeHidden()
  await expect(terminal(page)).toContainText('Thanks for the tip!')
})
