# Roadmap and Known Issues

Everything here is planned or broken, not built. What exists today is
described in [mudd/docs/design.md](../mudd/docs/design.md). This document
condenses the unbuilt parts of the original design specification, which git
history keeps in full (`design.md` at the repo root, removed after this
harvest).

Payments and wallets are parked in [blockchain.md](blockchain.md) and not
covered here.

## Known Issues

Verified limitations in the current code. Fix these before relying on the
affected feature.

### Multi-node coherence

`mudd` works single-node only.

- **No Raft RPC server.** `mudd/src/raft/network.rs` POSTs to
  `http://<peer>/raft/<endpoint>`, but no route serves `/raft/*`
  (`mudd/src/api/mod.rs`) and nothing binds `--raft-port`. Nodes started with
  `--peers` cannot reach each other.
- **Timers fire once per node.** Every node spawns the timer loop and loads
  the persisted call_outs (`mudd/src/api/mod.rs`, `run_timer_loop` in
  `mudd/src/api/scripting.rs`), so in a cluster each call_out and heartbeat
  would run on every node.
- **Per-node caches are never invalidated.** Access levels
  (`PermissionManager`, `mudd/src/permissions/mod.rs`) are cached per node
  and not updated when Raft applies another node's write.
- **Snapshots are held in memory whole.** A snapshot is built and sent as
  one in-memory buffer (the whole game database); streaming it in chunks
  comes with the cluster transport (`docs/cluster-design.md`).
- **No Raft RPC server.** `raft/network.rs` posts to `/raft/*` on peers, but
  no route serves those endpoints.

### Lua runtime

- **A verb may spend all of a player's credits.** Typing a verb on an
  object is consent to whatever its handler spends as `"actor"`; there is no
  per-call cap or confirmation prompt (`mudd/src/lua/game_api.rs`). Verb
  dispatch only hands over the wallet when the player clearly picked the
  object (`dispatch_custom_verb` in `mudd/src/api/websocket.rs`).
  Planned (Phase 6): charges to a player only through a confirmed payment:
  `game.request_payment(amount, reason, handler, data)` returns at once; the
  server sends `{type:"confirm", id, text, amount}`, the web client shows a
  dialog (text clients answer `yes`/`no`), and on acceptance before the
  deadline the server charges and runs the object's handler (as owner,
  payer = that player). Pending requests are rows with absolute deadlines.
  `"actor"` stops being a direct payer, including for paid Venice calls.

### Accounts and permissions

- **No rate limiting on login and register.** Concurrent argon2 work is
  capped at the CPU count (`mudd/src/auth/accounts.rs`), so a flood cannot
  exhaust memory, but it can still keep every core busy and delay real
  logins. Needs per-IP and per-username limits.

## Raft Cluster Operation

Today `mudd` runs a single-node openraft cluster whose log carries plain SQL
(`Request { sql, params }`). Planned:

- **Raft RPC endpoints** for append-entries, vote and install-snapshot,
  served on `--raft-port`.
- **Leader-only execution.** Only the leader runs Lua. A trigger (player
  command, timer, scheduled task) runs on the leader, which collects the
  resulting SQL mutations, proposes them, and broadcasts events to room
  occupants. Followers apply the SQL and never run Lua. Followers redirect
  or proxy client writes to the leader.
- **Typed log entries** instead of raw SQL:

  ```rust
  pub enum GameLogEntry {
      CreateUniverse { universe_id, name, owner_account_id, initial_config },
      DeleteUniverse { universe_id },
      Mutations { universe_id, sql_statements: Vec<String> },
      StoreCode { universe_id, hash, code, author_account_id },
  }
  ```

  One `Mutations` entry per execution would also make each Lua run's writes
  atomic (today each statement is its own entry).
- **Cold start**: a new node requests a snapshot from the leader (SQLite
  backup, compressed, streamed), applies it, replays log entries since the
  snapshot, and joins as a follower.
- **Failover**: target about 500 ms of disruption; in-flight commands may
  time out and the client retries; no committed data lost.
- **Cache coherence**: invalidate or bypass per-node caches when entries are
  applied (see Known Issues).
- **Node configuration file** (TOML), in addition to the CLI flags:

  ```toml
  node_id = 1
  peers = ["10.0.1.1:9000", "10.0.1.2:9000", "10.0.1.3:9000"]
  venice_api_key = "venice-..."
  venice_api_url = "https://api.venice.ai/api/v1"
  ```

- **Multi-node tests**: a cluster harness that starts several nodes and
  checks election, replication, snapshot transfer and failover.

## Storage

- **Per-universe databases**: a shared database for accounts and the
  universe registry, plus one database per universe, under a data directory
  (`raft/`, `shared.db`, `universes/*.db`). Today everything is in one file.
- Universe-scoped object IDs (see Known Issues).
- **Event log** table (event type, actor, target, room, data) for auditing
  and moderation.
- **Backups**: Raft replicates but is not an offsite backup; add periodic
  SQLite backups to object storage.
- **One durable commit per write, not two.** Each write now commits twice
  durably (the log append, then the apply), about 3.5 ms per write. As in
  the sister project roodb, only the log needs to be durable: apply with a
  non-syncing connection (`PRAGMA synchronous` is per connection) and let
  openraft re-apply from the log after a crash, with `save_committed` /
  `read_committed` on the log store and the applied position kept in the
  apply transaction (already the case). Needs a crash-recovery test.

## Economy and Limits

- **Credit costs for actions.** Draft prices, in US cents before conversion
  to credits:

  | Action | Cost |
  |--------|------|
  | Venice fast / smart / vision chat | 2¢ / 20¢ / 10¢ |
  | Venice image | 5¢ |
  | Room image (two-step) | 7¢ |
  | Object creation | 10¢ |
  | Bot monthly fee | 50¢ |
  | Class definition | $50 |

  Players pay credits to the universe; the operator pays Venice. Lua Venice
  calls already charge per-universe prices (`venice_chat_cost`,
  `venice_image_cost` in the universe config); the rest are unpriced.
- **Execution cost model** charging per instruction block, DB read, DB write,
  LLM tier, image and object creation.
- **Credit ledger**: a `credit_transactions` table (amount, type
  spend/refund/grant, spend category) and lifetime spend, instead of a bare
  balance.
- **Rate limits**: 60 commands per minute and 10 LLM calls per minute per
  account; 60 Venice calls per minute per bot. Today only Venice has a
  per-account limiter (60 requests per minute), and the sandbox caps calls
  per execution.

## Game Logic

The Lua game-logic suite (`mudd/tests/lua/game_logic_tests.lua`) skips tests
for features that do not exist yet; see its `SKIP` table. Planned:

- **Elemental combat**: `Combat.attack_extended` with weapon elemental dice
  and types, applying immunities, resistances and vulnerabilities, critical
  hits doubling dice, and a weapon `on_hit` handler.
- **Combat rounds and state**: `in_combat`, `attacking`, `attackers` and a
  round counter on living objects; heartbeat-driven rounds; auto-retaliation;
  combat stops when the target dies or leaves; picking a new target.
- **PvP policy enforcement** in `Combat.initiate`/`can_attack`: disabled,
  arena-only (`is_arena` rooms), flagged (`pvp_on`/`pvp_off` commands),
  open. Today the Rust `PvpPolicy` exists but nothing checks it.
- **Flee ends combat**: `Commands.flee` moves the player but there is no
  combat state to leave.
- **Status effects in combat**: stun and paralysis skip turns, stat
  modifiers apply to rolls, effects tick down per round
  (`Combat.process_statuses`).
- **NPC AI**: `ai_idle_tick` (aggro on sight, wandering) and
  `ai_combat_tick` (flee below `flee_threshold`, assist allies). Class `npc`
  only names these handlers.
- **Death handling**: player respawn at bind point with optional death
  penalty; NPC loot drop, XP award and scheduled respawn.
- **Equipment**: wield/unwield, wear/remove, armor slots.
- **Region hooks**: `on_enter`/`on_leave` on the region when a player
  crosses region boundaries.
- **An LPC-style `init()` cascade on moves.** `game.move_object` already
  runs the containers' `on_leave`/`on_enter`; add `on_init` between
  the newcomer and each object present, so objects can offer contextual
  verbs ("get sword", "wield sword") and remove them on `on_move`.
- **More lifecycle handlers**: `on_create`, `on_destroy`, `on_move`,
  `on_reset` (room respawn timer), `on_disconnect`.
- **Persistent actions**: registered verbs are in memory and lost on restart.
- **High-occupancy rooms**: above about 100 occupants, send an aggregated
  view (count, notable entities, sampled chat).
- **Player commands**: `who`, `quit`, `give`/`use` as server commands (the
  mudlib has `Commands.give` and `Commands.use`), global commands
  registered from Lua.

## Lua API

`game.*` functions from the original design that do not exist (the current
list is in [langref.md](../mudd/docs/langref.md)):

- Environment: `deep_inventory`, `present_living`, `get_parent` alias,
  `is_living`
- Actions and commands: `get_actions`, `register_command`,
  `unregister_command`
- Classes: `has_class`, `update_class`, `get_ancestors`, class handlers
  defined in `define_class` (only properties are kept today)
- Timers: `find_call_out`
- Status effects: `apply_status`, `remove_status`, `has_status`,
  `get_statuses`
- AI: `generate_room_image`
- Time and randomness: `random` (seeded per execution), `advance_time`
- Players: `disconnect`, `get_all_players`
- Logging: `log(level, message)` with rate limiting
- Simul-efuns loaded into every sandbox: `this_player()`, `this_object()`,
  `say`, `tell`, `shout`, `emote`

## Images and Themes

- Use the universe's `theme_id` for the welcome message and image prompts
  (today always `sierra-retro`).
- Charge credits for room images.
- Creator hints for the prompt step.
- Layering: background from Venice, object sprites (or Venice), client-side
  entity sprites and UI (names, health bars).
- Fallbacks when Venice is unavailable: procedural gradients or sprites.

## Web Client

The React client (`client/`) has auth, a universe picker, a room panel with
the room image and a terminal. Planned:

- Canvas room rendering with layered sprites.
- Typed room state (players, bots, objects, exits) instead of name lists.
- Status bar with HP and credit balance.
- Command history.

## HTTP API

Ideas from the original design, beyond today's `/auth/*` and `/universe/*`:

- REST universe resources: `GET /api/universes`, `POST /api/universes`,
  `GET /api/universes/:id`, and universe deletion.
- Account fields such as last login and bans.
- Cross-universe identity: the account as the global identity with friends,
  and whisper/mail between universes through shared messaging.

## Admin and Moderation

- Ban and kick players; account management.
- Universe deletion and archival.
- Moderation views over the event log.
- Monitoring: Prometheus metrics (Raft state, WebSocket connections, Lua
  execution time, Venice latency) with Grafana; alerts for no leader over
  5 s, replication lag over 1,000 entries, slow Lua, Venice errors.

## Deployment

Today there is a single-container Docker image (`Dockerfile`, `docker/`):
one `mudd` serving the client and the API. Planned:

- Docker Compose for a three-node cluster plus the client.
- A script to start a local three-node cluster.
- Production deployment and performance work.
