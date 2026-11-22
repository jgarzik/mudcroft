# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Mudcroft is a multi-user dungeon (MUD) game server in Rust (`mudd/`) with a React web client (`client/`), featuring:
- Sandboxed Lua scripting for programmable game mechanics
- LPC-style object system with path-based IDs and class inheritance
- WebSocket-based multiplayer gameplay
- Raft (openraft) log over SQLite; single node by default; clusters join over a TLS cluster port, the leader (primary) runs the game and other nodes are gateways (`docs/cluster-design.md`)

Design it as what it is: a specialized database server. The tables (written only through Raft) are the source of truth; in-memory maps are caches derived from them, never a second copy of state. One logical change (a combat round, an effect tick) is one atomic batch. State that lives only in memory is a flaw to fix. Prefer absolute deadlines and event-driven work to periodic sweeps: write only when something happens.

## Build & Test Commands

Server commands run from the `mudd/` directory:

```bash
cargo build --release                # Build mudd, mudd_init and mudd_admin
cargo test --lib                     # Unit tests only (fast)
cargo test --release                 # All tests including integration and Lua suite
cargo test --release test_name       # Run single test
cargo clippy --all-targets           # Lint check
cargo fmt --check                    # CI checks formatting

# A database must exist first; mudd never creates one
MUDD_ADMIN_USERNAME=admin MUDD_ADMIN_PASSWORD=testpass123 \
  cargo run --bin mudd_init -- --database "$DB" --lib-dir lib
cargo run --bin mudd -- --bind 127.0.0.1:8080 --database "$DB" [--static-dir ../client/dist]
```

Use a fresh private path for `$DB` (e.g. under `mktemp -d`), never a shared fixed path like `/tmp/mudcroft.db`: concurrent runs and tests would collide. Without `--lib-dir lib` the core mudlib (take/drop/inventory) is missing.

Client and end-to-end:

```bash
cd client && npm run lint            # eslint, zero warnings
cd client && npm run build           # tsc + vite build into client/dist
scripts/e2e.py                       # builds mudd + client, runs Playwright (headless Chromium)
cd client && npm run test:e2e        # Playwright only (needs built binaries and client/dist)
scripts/testsrv.py start             # manual play-test server with a seeded world
```

## Architecture

### Core Components (mudd/src/)

- `main.rs` - mudd CLI; `lib.rs` - Config, Server (DB open, Raft setup, 10s leader wait, serve)
- `bin/init.rs` - mudd_init CLI; `init.rs` - database creation/upgrade, admin account, mudlib load
- `bin/admin.rs` - mudd_admin CLI (offline, server stopped); `admin.rs` - global access levels
- `player.rs` - PlayerManager: persistent player objects, spawn, disconnect grace period
- **api/** - HTTP/WebSocket endpoints via Axum
  - `mod.rs` - AppState, router, startup loading, `--static-dir` serving
  - `auth.rs` - /auth/* endpoints (register, login, logout, validate)
  - `universe.rs` - /universe/* endpoints (list, create, upload, run_script)
  - `images.rs` - /images/{hash}
  - `gateway.rs` - on a non-primary node: forwards /auth and /universe to the primary, relays game sessions (follows failover)
  - `primary.rs` - primary lifecycle: only the Raft leader runs the timer loop and effect scheduler; caches rebuilt from tables on becoming primary
  - `scripting.rs` - script runner: per-execution sandbox + MessageQueue, hooks, verbs, timer loop, message delivery
  - `websocket.rs` - /ws endpoint, ConnectionManager, PlayerSession, command dispatch
- **auth/** - AccountService, tokens, password hashing
- **db/** - SQLite pool (WAL, foreign keys), migrations, validation
- **universe/** - universe ID validation
- **objects/** - LPC-style object system
  - `object.rs`, `path.rs` - Object, path-based IDs
  - `store.rs` - ObjectStore CRUD operations
  - `class.rs` - ClassDef, ClassRegistry, inheritance
- **lua/** - Lua sandbox with metering
  - `sandbox.rs` - Execution with instruction/memory limits and per-execution call budgets
  - `game_api.rs` - Exposed game.* functions
  - `libs.rs` - universe library loading (config `lib_hashes`)
  - `actions.rs` - ActionRegistry: cache of the `actions` table (custom verbs)
  - `messaging.rs` - MessageQueue
- **combat/** - dice, damage types, status effects, `engine` (attack resolution over object rows: HP lives in the object's `hp` property), PvP policies
- **raft/** - openraft storage/state machine (log entries are SQL requests; snapshots are game-table copies), `transport` (TLS cluster port: RPCs, join), RaftWriter
- **timers/** - call_out (one-shot) and heartbeat (recurring), both persisted (`timers`, `heartbeats`)
- **permissions/** - LPMud-style path-based access control
  - Access levels: player < builder < wizard < admin < owner, per universe (`universe_members`); global admin/owner apply everywhere; a universe's `owner_id` is owner inside it
  - Path grants: `/d/forest` grants access to all objects under that prefix
  - Ownership: creators can always modify their own objects
- **credits/** - per-universe balances
- **venice/** - AI integration (LLM chat, image generation)
- **images/** - content-addressed image store, room image generation
- **theme/** - theme registry for image prompts and client styling

### Security Model (must respect)

- Object code (hooks, verbs, timers, `game.use_object`) runs as the **object's owner**, never as the triggering player; the player is only data (`actor_id`). Don't pass a player's identity into object code execution. `actor_id` given to `use_object` is caller-supplied and untrusted.
- The one exception is credits: a verb handler gets the typing player's account as **payer** (`ScriptContext.payer`, Lua `"actor"`), and only when the player clearly picked that object; eval/run_script pay as their caller. Hooks, timers and nested `use_object` never get a payer. The payer can only be spent from (credits, Venice prices), never used for permissions.
- Anything that makes code run as an owner needs owner authority: attaching `code_hash` and `add_action`/`remove_action` require being the owner or outranking them (`can_act_as`). Private handlers (`_name`) and engine hooks (`lua::is_private_handler`) are never reachable via `use_object` or verbs. Class `code_hash` defaults are never attached to instances.
- `use_object` is bounded: 8 levels deep, 32 calls per top-level execution.
- Lua object writes (`create/update/delete/move/clone_object`) are permission-checked against the acting user; keep new `game.*` writes checked the same way and charge the call budget (`charge_db` / `charge_venice`).
- Never splice user input into Lua source. Pass values with `Sandbox::execute_with_args` (the chunk's `...`).
- Keep object code within the session's universe. Object ids are scoped per universe (`objects` PK `(universe_id, id)`), and every `ObjectStore` call, in-memory map (sessions, timers, actions, classes) and message delivery takes the universe. Thread the universe through any new code rather than keying by object id alone.

### Database

One SQLite file with tables: accounts, universes, universe_settings, objects, code_store, core_lib_hashes, classes, class_properties, class_handlers, universe_members, credits, timers, heartbeats, actions, cluster_nodes, path_grants, active_effects, image_store, raft_log, raft_vote, raft_meta. Schema source: `mudd/src/db/mod.rs`; see `SCHEMA.md`.

### WebSocket Protocol

- Connect: `/ws?universe=<id>&token=...` (token required; 401 without a valid one, no guests)
- Universe ID required (DNS-style: 3-64 chars, lowercase alphanumeric and hyphens)
- Client sends `{type:"command", text}` or `{type:"ping"}`; server sends `welcome`, `room`, `output`, `error`, `echo` (every command is echoed first), `replaced` (a newer connection for the same player took over; clients must not auto-reconnect)
- Commands: look, north/south/east/west/up/down (and n/s/e/w/u/d), say, get/take, drop, inventory/i, attack/kill, help; builder+: create; wizard+: eval, goto, setportal; other verbs dispatch to `game.add_action` handlers

## Test Infrastructure

`mudd/tests/harness/` provides:
- **TestServer** - spawns the real mudd binary in a temp dir (fresh DB via mudd_init); the child is killed on drop; logs go to the temp dir (`log_path`); `start_with_args` passes extra flags; `restart()` stops and restarts mudd on the same DB (`stop()`/`start_again()` to act on the DB in between); readiness waits for the server's own "listening" log line, and startup retries on a new port if the server exits
- **TestClient** - Role-based authenticated WebSocket client; `connect` waits for and queues the spawn room message
- **TestWorld** - Pre-configured universe with regions/rooms

Scenario tests live in `mudd/tests/scenarios/`; the Lua game-logic suite is `mudd/tests/lua/game_logic_tests.lua` (run by `lua_game_logic_test.rs`; unbuilt features are in its `SKIP` table). Browser tests are in `client/e2e/`.

## Key Design Documents

- `mudd/docs/design.md` - Server architecture (current state)
- `docs/` - Roadmap and known issues (`docs/roadmap.md`), multi-node design proposal (`docs/cluster-design.md`), parked blockchain design, research
- `mudd/README.md` - Server build and CLI
- `SCHEMA.md` - Database schema
- `playtest.md` - Manual play-testing
- `igor-history.md` - LP-MUD theory and historical context
- `mudd/docs/langtutor.md` - Content creator tutorial (Lua, rooms, NPCs, permissions)
- `mudd/docs/langref.md` - Lua API reference
- `mudd/docs/permissions.md` - Permission system technical reference
- `mudd/docs/API.md` - HTTP/WebSocket API reference
- `mudd/docs/admin.md` - Administration guide
