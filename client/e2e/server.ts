/**
 * Throwaway mudd server for end-to-end tests.
 *
 * Creates a temp dir with a fresh database (mudd_init), starts mudd on a
 * free port serving client/dist, and seeds a universe with the cave
 * adventure. Everything the server writes lives in the temp dir; stop()
 * kills the server's process group and deletes it.
 */
import { spawn, spawnSync, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'

export const CLIENT_DIR = path.resolve(import.meta.dirname, '..')
export const REPO_DIR = path.resolve(CLIENT_DIR, '..')
const MUDD_DIR = path.join(REPO_DIR, 'mudd')
const BIN_DIR = process.env.MUDD_BIN_DIR ?? path.join(MUDD_DIR, 'target', 'release')

export const ADMIN = { username: 'e2e_admin', password: 'e2e-admin-pass' }
export const UNIVERSE = { id: 'e2e-world', name: 'E2E World' }
export const PORTAL = '/rooms/cave-entrance'

/** What global setup hands to tests and teardown */
export interface ServerState {
  pid: number
  tmpDir: string
  url: string
  logPath: string
}

export const STATE_ENV = 'MUDCROFT_E2E_STATE'
export const URL_ENV = 'MUDCROFT_E2E_URL'

function requireFile(file: string, hint: string) {
  if (!fs.existsSync(file)) {
    throw new Error(`${file} not found. ${hint}`)
  }
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer()
    srv.unref()
    srv.on('error', reject)
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address()
      srv.close(() => (typeof addr === 'object' && addr ? resolve(addr.port) : reject(new Error('no port'))))
    })
  })
}

function logTail(logPath: string, lines = 40): string {
  try {
    return fs.readFileSync(logPath, 'utf8').split('\n').slice(-lines).join('\n')
  } catch {
    return '<no log>'
  }
}

async function waitForHealth(url: string, child: ChildProcess, logPath: string) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`mudd exited during startup (${child.exitCode}):\n${logTail(logPath)}`)
    }
    try {
      const res = await fetch(`${url}/health`)
      if (res.ok) return
    } catch {
      // not listening yet
    }
    await new Promise((r) => setTimeout(r, 100))
  }
  throw new Error(`mudd did not become healthy:\n${logTail(logPath)}`)
}

async function api(url: string, method: string, route: string, body?: unknown, token?: string) {
  const res = await fetch(`${url}${route}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await res.text()
  if (!res.ok) throw new Error(`${method} ${route} -> ${res.status}: ${text}`)
  return text ? JSON.parse(text) : {}
}

/** Send one command over the game WebSocket and wait for its reply */
async function wsCommand(url: string, token: string, command: string): Promise<string> {
  const wsUrl = `${url.replace(/^http/, 'ws')}/ws?universe=${UNIVERSE.id}&token=${token}`
  const ws = new WebSocket(wsUrl)
  return await new Promise<string>((resolve, reject) => {
    const timer = setTimeout(() => {
      ws.close()
      reject(new Error(`no reply to ${command}`))
    }, 10_000)
    // The reply is the first output/error after our command is echoed back
    // (the server may send other output first, e.g. a greeting)
    let sent = false
    let echoed = false
    ws.onmessage = (ev) => {
      const msg = JSON.parse(String(ev.data))
      if (msg.type === 'welcome' && !sent) {
        sent = true
        ws.send(JSON.stringify({ type: 'command', text: command }))
      } else if (msg.type === 'echo') {
        echoed = true
      } else if (echoed && (msg.type === 'output' || msg.type === 'error')) {
        clearTimeout(timer)
        ws.close()
        if (msg.type === 'error') reject(new Error(`${command}: ${msg.message}`))
        else resolve(msg.text)
      }
    }
    ws.onerror = () => {
      clearTimeout(timer)
      reject(new Error(`WebSocket error for ${command}`))
    }
  })
}

async function seed(url: string) {
  const login = await api(url, 'POST', '/auth/login', ADMIN)
  await api(
    url,
    'POST',
    '/universe/create',
    { id: UNIVERSE.id, name: UNIVERSE.name, owner_id: login.account_id },
    login.token,
  )
  await api(url, 'POST', `/universe/${UNIVERSE.id}/run_script`, { script: 'cave_adventure.lua' }, login.token)
  await wsCommand(url, login.token, `setportal ${PORTAL}`)
}

/** Start and seed a server. Throws (after cleaning up) on failure. */
export async function startServer(): Promise<ServerState> {
  const dist = path.join(CLIENT_DIR, 'dist')
  requireFile(path.join(dist, 'index.html'), 'Build the client first: npm run build')
  requireFile(path.join(BIN_DIR, 'mudd'), 'Build mudd first: (cd mudd && cargo build --release)')

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'mudcroft-e2e-'))
  const dbPath = path.join(tmpDir, 'e2e.db')
  const logPath = path.join(tmpDir, 'mudd.log')

  try {
    const init = spawnSync(
      path.join(BIN_DIR, 'mudd_init'),
      ['--database', dbPath, '--lib-dir', path.join(MUDD_DIR, 'lib')],
      {
        env: { ...process.env, MUDD_ADMIN_USERNAME: ADMIN.username, MUDD_ADMIN_PASSWORD: ADMIN.password },
        encoding: 'utf8',
      },
    )
    if (init.status !== 0) {
      throw new Error(`mudd_init failed (${init.status}):\n${init.stdout}\n${init.stderr}`)
    }

    // Retry on the rare port collision between picking and binding
    for (let attempt = 1; ; attempt++) {
      const port = await freePort()
      const url = `http://127.0.0.1:${port}`
      const log = fs.openSync(logPath, 'a')
      const child = spawn(
        path.join(BIN_DIR, 'mudd'),
        ['--bind', `127.0.0.1:${port}`, '--database', dbPath, '--static-dir', dist],
        {
          // run_script reads scripts/ relative to the working directory
          cwd: REPO_DIR,
          // Own process group, so teardown can kill everything it started
          detached: true,
          stdio: ['ignore', log, log],
          env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? 'info' },
        },
      )
      fs.closeSync(log)
      const state: ServerState = { pid: child.pid!, tmpDir, url, logPath }
      try {
        await waitForHealth(url, child, logPath)
        await seed(url)
        return state
      } catch (err) {
        killGroup(state.pid)
        if (attempt >= 3 || !String(err).includes('exited during startup')) throw err
      }
    }
  } catch (err) {
    fs.rmSync(tmpDir, { recursive: true, force: true })
    throw err
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(-pid, 0)
    return true
  } catch {
    return false
  }
}

/** SIGTERM the server's process group, then SIGKILL whatever is left */
export function killGroup(pid: number) {
  try {
    process.kill(-pid, 'SIGTERM')
  } catch {
    return // already gone
  }
  const deadline = Date.now() + 3_000
  while (alive(pid) && Date.now() < deadline) {
    // Synchronous wait so this also works from an 'exit' handler
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 50)
  }
  if (alive(pid)) {
    try {
      process.kill(-pid, 'SIGKILL')
    } catch {
      // gone
    }
  }
}

/** Stop the server and delete its temp dir (kept if MUDCROFT_E2E_KEEP=1) */
export function stopServer(state: ServerState) {
  killGroup(state.pid)
  if (process.env.MUDCROFT_E2E_KEEP === '1') {
    console.log(`Keeping e2e temp dir: ${state.tmpDir}`)
  } else {
    fs.rmSync(state.tmpDir, { recursive: true, force: true })
  }
}
