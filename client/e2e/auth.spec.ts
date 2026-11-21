import { test, expect, register, uniqueName, PASSWORD } from './fixtures'

test('register, log out and log back in', async ({ page }) => {
  const name = uniqueName('auth')
  await register(page, name)

  // Logging out from the picker returns to the auth screen
  await page.getByRole('button', { name: 'Logout' }).click()
  await expect(page.getByTestId('auth-username')).toBeVisible()

  await page.getByTestId('auth-username').fill(name)
  await page.getByTestId('auth-password').fill(PASSWORD)
  await page.getByTestId('auth-submit').click()
  await expect(page.getByTestId('universe-list')).toBeVisible()
  await expect(page.getByTestId('universe-item').filter({ hasText: 'E2E World' })).toBeVisible()
})

test('wrong password is rejected', async ({ page }) => {
  const name = uniqueName('badpw')
  await register(page, name)
  await page.getByRole('button', { name: 'Logout' }).click()

  await page.getByTestId('auth-username').fill(name)
  await page.getByTestId('auth-password').fill('not-the-password')
  await page.getByTestId('auth-submit').click()
  await expect(page.getByTestId('auth-error')).toBeVisible()
  await expect(page.getByTestId('universe-list')).toHaveCount(0)
})

test('session survives a page reload', async ({ page }) => {
  await register(page, uniqueName('reload'))
  await page.reload()
  await expect(page.getByTestId('universe-list')).toBeVisible()
})
