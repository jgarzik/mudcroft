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

Clusters work as described in `docs/cluster-design.md` (joining, the
primary lifecycle, gateways, failover), with these gaps:

- **Commands are not exactly-once across a failover.** A command in flight
  when the primary dies may or may not have taken effect; the player is told
  and nothing is retried (one batch per command and command ids would fix
  it).
- **Snapshots are held in memory whole.** A snapshot travels in 1 MiB
  chunks, but is built, sent and installed from one in-memory buffer (the
  whole game database) on each side (`mudd/src/raft/storage.rs`,
  `transport.rs`).

## Raft Cluster Operation

Clusters are built (see [cluster-design.md](cluster-design.md)): `--join`
membership, a TLS cluster port with a shared secret, a single primary that
runs the game, gateways that relay to it, and failover. Planned:

- **One batch per Lua run.** Each `game.*` write is its own Raft entry
  today. Collecting a whole execution's writes into one entry would make
  each command atomic and, with a command id recorded in the batch, let a
  gateway retry a command interrupted by a failover exactly once. Typed log
  entries could replace raw SQL at the same time:

  ```rust
  pub enum GameLogEntry {
      CreateUniverse { universe_id, name, owner_account_id, initial_config },
      DeleteUniverse { universe_id },
      Mutations { universe_id, command_id, sql_statements: Vec<String> },
      StoreCode { universe_id, hash, code, author_account_id },
  }
  ```

- **Streamed snapshots** built and installed without holding the whole
  database in memory (see Known Issues).
- **Node configuration file** (TOML), in addition to the CLI flags.

## Storage

- **Per-universe databases**: a shared database for accounts and the
  universe registry, plus one database per universe, under a data directory
  (`raft/`, `shared.db`, `universes/*.db`). Today everything is in one file.
- **Event log** table (event type, actor, target, room, data) for auditing
  and moderation.
- **Backups**: Raft replicates but is not an offsite backup; add periodic
  SQLite backups to object storage.

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

- Docker Compose for a three-node cluster plus the client (with
  operator-supplied cluster certificates and secret).
- A script to start a local three-node cluster.
- Production deployment and performance work.
