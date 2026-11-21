import { test, expect, register, enterWorld, command, terminal, uniqueName } from './fixtures'

test.beforeEach(async ({ page }) => {
  await register(page, uniqueName('play'))
  await enterWorld(page)
})

test('the room panel shows the spawn room', async ({ page }) => {
  await expect(page.getByTestId('room-description')).toContainText('dark opening in the mountainside')
  await expect(page.getByTestId('room-exits')).toHaveText('north')
  await expect(page.getByTestId('room-contents')).toContainText('Rusty Short Sword')
})

test('moving updates the room panel', async ({ page }) => {
  await command(page, 'north')
  await expect(page.getByTestId('room-name')).toHaveText('Narrow Passage')
  await expect(page.getByTestId('room-exits')).toContainText('east')

  await command(page, 'east')
  await expect(page.getByTestId('room-name')).toHaveText('Underground Pool')

  await command(page, 'west')
  await command(page, 'south')
  await expect(page.getByTestId('room-name')).toHaveText('Cave Entrance')
})

test('blocked directions are reported', async ({ page }) => {
  await command(page, 'west')
  await expect(terminal(page)).toContainText("You can't go that way.")
  await expect(page.getByTestId('room-name')).toHaveText('Cave Entrance')
})

test('commands are echoed in the terminal', async ({ page }) => {
  await command(page, 'help')
  await expect(terminal(page)).toContainText('> help')
  await expect(terminal(page)).toContainText('Commands:')
})

test('take, inventory and drop', async ({ page }) => {
  // Players share the world, so another test may hold the sword: only
  // assert on our own actions
  await command(page, 'take sword')
  const took = terminal(page).getByText('You take Rusty Short Sword.')
  const gone = terminal(page).getByText("You don't see that here")
  await expect(took.or(gone)).toBeVisible()
  test.skip(await gone.isVisible(), 'sword taken by a parallel test')

  await command(page, 'inventory')
  await expect(terminal(page)).toContainText('Rusty Short Sword')

  await command(page, 'drop sword')
  await expect(terminal(page)).toContainText('You drop Rusty Short Sword.')
})
