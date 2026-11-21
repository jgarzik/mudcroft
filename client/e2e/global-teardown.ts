import { stopServer, STATE_ENV, type ServerState } from './server'

/** Stop the server started by global setup and delete its temp dir */
export default async function globalTeardown() {
  const raw = process.env[STATE_ENV]
  if (!raw) return
  stopServer(JSON.parse(raw) as ServerState)
  delete process.env[STATE_ENV]
}
