import { startServer, stopServer, STATE_ENV, URL_ENV } from './server'

/**
 * Start the throwaway mudd server and publish its URL to the tests.
 * Environment variables set here are inherited by the test workers.
 */
export default async function globalSetup() {
  const state = await startServer()
  process.env[URL_ENV] = state.url
  process.env[STATE_ENV] = JSON.stringify(state)

  // Last line of defence: if the runner exits without reaching global
  // teardown (Ctrl-C, crash), still stop the server and delete the temp dir.
  // Teardown clears the env var once it has cleaned up.
  process.once('exit', () => {
    if (process.env[STATE_ENV]) stopServer(state)
  })
  for (const signal of ['SIGINT', 'SIGTERM'] as const) {
    process.once(signal, () => {
      if (process.env[STATE_ENV]) stopServer(state)
      delete process.env[STATE_ENV]
      process.exit(130)
    })
  }

  console.log(`mudd e2e server: ${state.url} (temp dir ${state.tmpDir})`)
}
