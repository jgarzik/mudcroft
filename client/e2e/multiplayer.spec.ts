import { test, expect, command, terminal, newPlayer, enterWorld } from './fixtures'

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

test('a second tab takes over without the two fighting', async ({ browser }) => {
  const first = await newPlayer(browser, 'tabs')
  try {
    // Same account (the session is shared by the context) in a second tab
    const second = await first.context.newPage()
    await second.goto('/')
    await enterWorld(second)

    await expect(terminal(first.page)).toContainText('another window')
    await expect(first.page.getByTestId('connection-status')).toHaveText('disconnected')
    // The first tab does not reconnect and evict the second
    await first.page.waitForTimeout(2500)
    await expect(first.page.getByTestId('connection-status')).toHaveText('disconnected')
    await expect(second.getByTestId('connection-status')).toHaveText('connected')
  } finally {
    await first.context.close()
  }
})
