# Mudcroft

Mudcroft is a multi-user dungeon (MUD) server and browser client. The world
is programmable: rooms, items and NPCs carry sandboxed Lua code, in the
tradition of LP-MUD, where builders and wizards extend the game from inside
it. The server, `mudd`, is written in Rust; the client is React.

## Features

- **Sandboxed Lua scripting**: every run gets a fresh Lua 5.4 sandbox with
  instruction, memory and per-execution database/AI call limits. Object code
  runs as the object's owner, and Lua writes are permission-checked.
- **LPC-style objects**: path-based object IDs (`/rooms/dark-cave`), class
  inheritance with default properties, content-addressed code, and hooks
  (`on_enter`, `on_leave`, `on_damage`, `on_death`), custom verbs,
  `call_out` timers and heartbeats.
- **Permissions**: player, builder, wizard, admin and owner levels, path
  grants for builders, and ownership.
- **Multiple universes** on one server, each with its own Lua libraries,
  portal room and credit balances.
- **Combat**: d20 attacks, dice notation, damage types and status effects.
- **WebSocket play** with token authentication, plus a web client with a
  room panel and terminal.
- **AI rooms**: with a Venice AI key, room images are generated in the
  background (LLM writes the prompt, an image model draws it).
- **Raft log over SQLite**: writes go through openraft (single node today;
  multi-node clustering is on the [roadmap](docs/roadmap.md)).

## Repository Layout

| Path | Contents |
|------|----------|
| [mudd/](mudd/) | Rust server: `mudd` daemon, `mudd_init` database tool, `mudd_admin` offline admin tool, core mudlib (`mudd/lib/*.lua`), tests |
| [client/](client/) | React + TypeScript + Vite web client and Playwright end-to-end tests |
| [scripts/](scripts/) | Play-test server scripts, e2e runner, sample world (`cave_adventure.lua`) |
| [docs/](docs/) | Roadmap, known issues and parked designs |
| [docker/](docker/), [Dockerfile](Dockerfile) | Single-container image (nginx + mudd) |

## Quick Start

Prerequisites: a Rust toolchain, Node.js 22+ (the play-test script uses its
built-in `WebSocket`), and for the play-test script `curl` and `jq`.

The fastest way to play is the play-test script. It creates a throwaway
database, starts `mudd` serving the client, creates a universe and loads the
sample cave adventure:

```bash
(cd mudd && cargo build --release)
(cd client && npm ci && npm run build)
scripts/start-testsrv.sh        # prints the URL and admin login; Ctrl+C stops it
```

To run a server by hand:

```bash
cd mudd
cargo build --release

# Create a database with an admin account and the core mudlib
MUDD_ADMIN_USERNAME=admin MUDD_ADMIN_PASSWORD='change-me-please' \
  target/release/mudd_init --database ../mudcroft.db --lib-dir lib

# Serve the API and the built client on one port (build the client first)
target/release/mudd --database ../mudcroft.db \
  --bind 127.0.0.1:8080 --static-dir ../client/dist
```

Open <http://127.0.0.1:8080>. A new database has no universe: log in as the
admin over HTTP, create one with `POST /universe/create`, optionally load a
world with `POST /universe/{id}/run_script`, then connect as a wizard and set
the spawn room with `setportal`. [playtest.md](playtest.md) and
[mudd/docs/admin.md](mudd/docs/admin.md) walk through this, and
`scripts/start-testsrv.sh` shows it end to end.

## Tests

```bash
cd mudd
cargo test --release            # unit, integration and Lua game-logic tests
cargo clippy --all-targets

scripts/e2e.sh                  # builds everything, runs headless Chromium tests
(cd client && npm run lint)
```

Playwright's browser must be installed once: `cd client && npx playwright
install chromium`.

## Documentation

- [mudd/README.md](mudd/README.md): server build and CLI
- [mudd/docs/design.md](mudd/docs/design.md): server architecture
- [mudd/docs/langtutor.md](mudd/docs/langtutor.md): building worlds in Lua
- [mudd/docs/langref.md](mudd/docs/langref.md): Lua `game.*` API reference
- [mudd/docs/API.md](mudd/docs/API.md): HTTP and WebSocket API
- [mudd/docs/permissions.md](mudd/docs/permissions.md): permission system
- [mudd/docs/admin.md](mudd/docs/admin.md): administration
- [SCHEMA.md](SCHEMA.md): database schema
- [playtest.md](playtest.md): manual play-testing
- [docs/roadmap.md](docs/roadmap.md): planned work and known issues
- [igor-history.md](igor-history.md): LP-MUD background
