import { test, expect, command, terminal, newPlayer } from './fixtures'

test('players in the same room hear each other', async ({ browser }) => {
  const alice = await newPlayer(browser, 'alice')
  const bob = await newPlayer(browser, 'bob')
  try {
    await command(alice.page, 'say hello from alice')
    await expect(terminal(alice.page)).toContainText('You say: hello from alice')
    await expect(terminal(bob.page)).toContainText(`${alice.name} says: hello from alice`)
  } finally {
    await alice.context.close()
    await bob.context.close()
  }
})

test('players in other rooms do not', async ({ browser }) => {
  const alice = await newPlayer(browser, 'alice2')
  const carol = await newPlayer(browser, 'carol')
  try {
    await command(carol.page, 'north')
    await expect(carol.page.getByTestId('room-name')).toHaveText('Narrow Passage')

    await command(alice.page, 'say just between us')
    await expect(terminal(alice.page)).toContainText('You say: just between us')
    // Give the message time to (not) arrive
    await carol.page.waitForTimeout(500)
    await expect(terminal(carol.page)).not.toContainText('just between us')
  } finally {
    await alice.context.close()
    await carol.context.close()
  }
})
