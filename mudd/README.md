# mudd

`mudd` is the Mudcroft game server: a multi-user dungeon with sandboxed Lua
scripting, persisted in SQLite and written through a Raft log.

## Features

- **Sandboxed Lua:** per-execution limits on instructions, memory, wall time,
  database calls and Venice calls.
- **LPC-style objects:** path ids such as `/rooms/cave-entrance`, built-in base
  classes plus `game.define_class`.
- **Object code:** attach Lua to objects (`code_hash`). It runs as the object's
  owner:
  - room and NPC hooks: `on_enter`, `on_leave`, `on_damage`, `on_death`
  - custom verbs registered with `game.add_action`
  - `game.use_object`
- **Timers:** one-shot `game.call_out` (persisted) and recurring
  `game.set_heart_beat` (in memory).
- **Messaging:** `say`, plus `game.send`, `game.broadcast` and
  `game.broadcast_region` from Lua.
- **Combat:** dice rolls, damage types and status effects. The `attack`
  command strikes NPCs.
- **Permissions:** five access levels (player, builder, wizard, admin, owner),
  per-universe path grants and object ownership. See
  [docs/permissions.md](docs/permissions.md).
- **Multiplayer over WebSocket.** Authentication uses opaque random session
  tokens (not JWTs) and salted SHA-256 password hashes.
- **Web client:** `--static-dir` serves the built React client (`client/dist`)
  on the same origin.
- **Venice AI integration** (optional): LLM chat, image generation and
  automatic room images.
- **SQLite storage** in WAL mode. All writes go through OpenRaft. Only
  single-node mode works today; see
  [docs/admin.md](docs/admin.md#mudd).

## Quick Start

Run these commands from the `mudd/` directory:

```bash
# 1. Build the server, and the web client if you want it
cargo build --release
(cd ../client && npm ci && npm run build)

# 2. Create the database and load the core Lua library from lib/
DB="$(mktemp -d)/mudcroft.db"
MUDD_ADMIN_USERNAME=admin MUDD_ADMIN_PASSWORD=secretpass123 \
    ./target/release/mudd_init --database "$DB" --lib-dir lib

# 3. Start the server (stop and restart it on the same "$DB" at any time)
./target/release/mudd --database "$DB" --bind 127.0.0.1:8080 \
    --static-dir ../client/dist
```

Without `--lib-dir`, `mudd_init` looks for `./lib` and silently skips the core
library if it is not there. Universes created afterwards then lack `get`,
`drop` and `inventory`.

A new database has no universes. Create one with an admin token, then connect:

```bash
LOGIN=$(curl -s -X POST http://127.0.0.1:8080/auth/login \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"secretpass123"}')
TOKEN=$(echo "$LOGIN" | jq -r .token)
ADMIN_ID=$(echo "$LOGIN" | jq -r .account_id)

curl -s -X POST http://127.0.0.1:8080/universe/create \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"id\":\"my-game\",\"name\":\"My Game\",\"owner_id\":\"$ADMIN_ID\"}"

wscat -c "ws://127.0.0.1:8080/ws?universe=my-game&token=$TOKEN"
```

Or open `http://127.0.0.1:8080/` in a browser. The universe has no rooms until
you build some and set a portal; see
[docs/admin.md](docs/admin.md#building-and-opening-a-universe).

To get a server with a world already loaded, run `scripts/testsrv.py start`
from the repository root (see [../playtest.md](../playtest.md)).

All flags and environment variables for `mudd` and `mudd_init`, plus Docker
and systemd setups, are in [docs/admin.md](docs/admin.md).

## Tests

```bash
cargo test --lib             # unit tests (fast)
cargo test --release         # all tests, including integration tests that spawn mudd
cargo clippy --all-targets   # lint
```

For browser end-to-end tests (Playwright, headless Chromium), run
`scripts/e2e.py` from the repository root.

## HTTP and WebSocket Endpoints

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/` | GET | | Server name and version. Serves the web client instead when `--static-dir` is set. |
| `/health` | GET | | Health check |
| `/auth/register` | POST | | Create an account and get a token |
| `/auth/login` | POST | | Log in and get a token |
| `/auth/logout` | POST | | Invalidate a token |
| `/auth/validate` | GET | | Check a token (`?token=`) |
| `/universe/list` | GET | any token | List universes |
| `/universe/create` | POST | admin+ | Create a universe from JSON |
| `/universe/upload` | POST | admin+ | Create a universe from a ZIP file |
| `/universe/{id}/run_script` | POST | admin+ or universe owner | Run a file from `scripts/` in a universe |
| `/images/{hash}` | GET | | Fetch a stored image |
| `/ws` | GET | optional `token` | WebSocket (`?universe=<id>&token=<token>`) |

Details: [docs/API.md](docs/API.md).

## Documentation

- [docs/API.md](docs/API.md): HTTP and WebSocket API
- [docs/admin.md](docs/admin.md): installation, CLI reference, Docker, operations
- [docs/permissions.md](docs/permissions.md): access levels and permission checks
- [docs/langtutor.md](docs/langtutor.md): building content in Lua
- [docs/langref.md](docs/langref.md): Lua `game.*` API reference
- [../SCHEMA.md](../SCHEMA.md): database schema

## License

The repository does not include a license file. The Docker image declares
`org.opencontainers.image.licenses="MIT"`.
