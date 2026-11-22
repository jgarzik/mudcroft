# Server Design Guide

How `mudd` is built today. This is a current-state document: anything not yet
built is in [docs/roadmap.md](../../docs/roadmap.md), which also lists known
issues. Related references:

- [permissions.md](permissions.md): access levels, path grants, ownership
- [admin.md](admin.md): CLI flags, environment variables, operations
- [API.md](API.md): HTTP and WebSocket protocol
- [langref.md](langref.md): the `game.*` Lua API
- [SCHEMA.md](../../SCHEMA.md): database schema

## Architecture Overview

```
mudd/src/
├── main.rs          # mudd CLI (clap) -> Config -> Server
├── lib.rs           # Config, Server: DB open, Raft setup, leader wait, serve
├── bin/init.rs      # mudd_init CLI (create/upgrade a database)
├── init.rs          # init_database: migrations, admin account, mudlib load
├── player.rs        # PlayerManager: persistent player objects, spawn, disconnect
├── db/              # SQLite pool (WAL, foreign keys), migrations, validation
├── api/             # Axum router and handlers
│   ├── mod.rs       # AppState, router, startup loading, --static-dir
│   ├── auth.rs      # /auth/* endpoints
│   ├── universe.rs  # /universe/* endpoints, run_script
│   ├── images.rs    # /images/{hash}
│   ├── scripting.rs # script runner: sandboxes, hooks, timer loop, delivery
│   └── websocket.rs # /ws, ConnectionManager, PlayerSession, commands
├── auth/            # AccountService, tokens, password hashing
├── universe/        # Universe ID validation
├── objects/         # Object, ObjectStore, ClassRegistry, path validation
├── lua/
│   ├── sandbox.rs   # Sandbox, SandboxConfig, call budgets
│   ├── game_api.rs  # game.* functions and parent()
│   ├── libs.rs      # universe library loading
│   ├── actions.rs   # ActionRegistry (custom verbs)
│   ├── messaging.rs # MessageQueue (per execution)
│   └── metering.rs  # instruction/memory counters
├── combat/          # dice, damage types, status effects, engine (attacks), PvpPolicy
├── permissions/     # PermissionManager, AccessLevel, path grants
├── timers/          # TimerManager: call_out, heartbeats
├── credits/         # CreditManager: per-universe balances
├── venice/          # Venice AI client (chat, images, rate limiter)
├── images/          # ImageStore (content-addressed), room image generation
├── theme/           # ThemeRegistry (sierra-retro, modern)
└── raft/            # openraft: storage, state machine, network, writer
```

`mudd/lib/*.lua` is the core mudlib (`combat.lua`, `commands.lua`,
`utils.lua`), loaded into the database by `mudd_init --lib-dir`.

## Data Model

One SQLite database holds everything: accounts, every universe, the Raft log
and Raft metadata. Tables are created by `Database::run_migrations`
(`src/db/mod.rs`); see [SCHEMA.md](../../SCHEMA.md) for columns.

| Group | Tables |
|-------|--------|
| Accounts and worlds | `accounts`, `universes`, `universe_settings` (e.g. the portal room) |
| Objects and code | `objects`, `code_store`, `core_lib_hashes` |
| Classes | `classes`, `class_properties`, `class_handlers` |
| Gameplay | `credits`, `timers`, `active_effects` |
| Permissions | `path_grants` |
| Images | `image_store` |
| Raft | `raft_log`, `raft_vote`, `raft_meta` |

```
accounts(id) <-- universes(owner_id), path_grants(grantee_id)
universes(id) <-- objects(universe_id), credits, timers, path_grants, universe_settings
objects(id)   <-- objects(parent_id)       -- containment
code_store(hash) <-- objects(code_hash), universe config lib_hashes
```

`mudd` opens an existing database only (`Database::open`): it checks that the
required tables exist and that at least one admin or owner account exists, and never
runs migrations; it also refuses a database whose schema version
(`PRAGMA user_version`) is not `SCHEMA_VERSION`. `mudd_init` creates or
upgrades a database (see [admin.md](admin.md)); it is the only place schema
upgrades live (`Database::upgrade`), and there are none yet.

## Startup Sequence

1. Parse CLI args: `--bind`, `--database`, `--static-dir`, and for a
   cluster member `--advertise`, `--cluster-*` and `--join`
   ([admin.md](admin.md) has details).
2. If `--static-dir` is set, fail unless it contains `index.html`.
3. Open and validate the database.
4. Start Raft (`RaftWriter::start`). The node's id is kept in its own
   `raft_meta` (never in a snapshot). A node resumes with that id; a new
   node given `--join` asks the cluster for an id first; otherwise it is
   node 1 and initializes a one-member cluster (once: on a database it has
   served before it resumes the persisted vote, log and membership). A
   cluster member also serves the cluster port.
5. Wait for a Raft leader (`wait_for_leader`: 10 s, or 120 s for a cluster
   member, which may first receive a snapshot). The HTTP port is not bound
   until this succeeds.
6. Build `AppState` (`api::router`): `ConnectionManager`, `ObjectStore`,
   `ClassRegistry`, `ActionRegistry`, `PermissionManager`, `PlayerManager`,
   `TimerManager`, `CreditManager`, `VeniceClient`, `ImageStore`,
   `ThemeRegistry`.
7. Start the primary lifecycle (`api/primary.rs`). Only the Raft leader,
   the primary, runs the timer loop and the effect scheduler. On becoming
   primary a node confirms leadership with a quorum and applies everything
   committed before its term (`ensure_linearizable(ReadIndex)`), rebuilds
   its caches from the tables (timers and heartbeats, access levels and
   path grants, custom classes, status effects, custom verbs), then starts
   both loops;
   on losing leadership it stops them and drops its caches of custom verbs.
   Deadlines are absolute, so whatever fell due during
   a failover runs at once, and only once. After the reconnect grace
   period it also handles the disconnect of every player recorded in
   `online_players` who has not reconnected (their server crashed or
   stepped down). A node that is leader at startup (always, for a single
   node) waits for this before serving. Elsewhere,
   writes fail with "not the primary".
8. Bind the TCP listener and serve the Axum router with graceful shutdown.

## HTTP Surface

| Route | Handler |
|-------|---------|
| `GET /health` | DB health check |
| `GET /ws` | WebSocket upgrade |
| `/auth/register`, `/auth/login`, `/auth/logout`, `/auth/validate` | `api/auth.rs` |
| `GET /universe/list`, `POST /universe/create`, `POST /universe/upload`, `POST /universe/{id}/run_script` | `api/universe.rs` |
| `GET /images/{hash}` | `api/images.rs` (immutable cache headers) |
| `GET /` | JSON server info, or the web client with `--static-dir` |

With `--static-dir <dir>`, `mudd` serves the built web client from the same
origin as the API: files by path, and `index.html` for `/` and for any path no
API route claims (client-side routes). This removes the need for the Vite dev
proxy or nginx. Request and response formats are in [API.md](API.md).

Universe creation (admin only) seeds the universe config's `lib_hashes` with
the core mudlib hashes from `core_lib_hashes`, plus any libraries in the
request. `run_script` (admin or universe owner) reads a file from `scripts/`
relative to the server's working directory and runs it with the eval limits
below. An admin may run it as an account strictly below their own level.

## WebSocket Sessions

```
Client -> /ws?universe=<id>&token=<token>
  ws_handler:
    1. Validate the universe ID (3-64 chars, lowercase alphanumeric and hyphens)
    2. Check the universe exists (404 otherwise)
    3. Validate the token (401 if missing or invalid; there are no guests)
  handle_socket:
    1. Create a bounded outgoing queue (256 messages)
    2. Player id /players/p-<account_id>: get or create the player object
       in this universe, spawn at last safe location or the universe portal
    3. Register the PlayerSession (player, account, universe, room, sender);
       the access level is read per command, not stored
    4. Send welcome, then the room description, then run on_enter on the
       spawn room (or tell wizards to use setportal if there is no portal)
    5. Loop: client commands in, queued server messages out
  on disconnect:
    unregister immediately; for accounts, run PlayerManager::handle_disconnect
    after a 5 second grace period unless the player reconnects first
```

Message types: client sends `command` and `ping`; server sends `welcome`,
`output`, `room` (name, description, exits, contents, optional `image_hash`),
`error`, `echo` and `replaced` (a newer connection for the same player took
over; the client does not auto-reconnect).

### Delivery

All messages to a client go through its bounded `mpsc` queue with `try_send`.
`ConnectionManager` copies recipients out of the session map before
delivering, so it never holds the lock while sending. A full or closed queue
drops the message with a warning: a client that stops reading only loses its
own messages and never stalls broadcasters, scripts or other sessions.

### Commands

Each command is echoed back, executed, and the reply sent. Hooks that should
follow the reply (such as `on_enter` after the new room description) are
deferred until the reply is queued. Every command gets a reply, even an
empty one, except a custom verb whose handler returns `nil`
(`ServerMessage::Silent`, never sent). `eval` and `create` take everything
after the verb as their argument.

| Command | Access | Behavior |
|---------|--------|----------|
| `look`, `l` | all | Room description |
| `north`/`n`, `south`/`s`, `east`/`e`, `west`/`w`, `up`/`u`, `down`/`d` | all | Follow exit; `on_leave` on the old room, `on_enter` on the new one |
| `say <msg>` | all | `<name> says: ...` to others in the room |
| `get`/`take <item>`, `drop <item>`, `inventory`/`inv`/`i` | all | Call `Commands.take/drop/inventory` from the mudlib; `take` refuses `fixed` and living objects, and item names match literally (`%`, `_` escaped) |
| `attack`/`kill <target>` | all | Rust combat against an NPC (`npc`, an `npc` subclass, or `monster`) |
| `help` | all | Commands for the caller's access level |
| `create <class> <path> "<name>" ["<desc>"] [key=value ...]` | builder+ | Create an object, path-permission checked |
| `eval <lua>` | wizard+ | Run Lua as the caller |
| `goto <room_id>` | wizard+ | Teleport (same universe only), with hooks; moves the player object too |
| `setportal [room_id]` | wizard+ | Set the universe spawn room (same universe only) |
| anything else | all | Custom verb dispatch, else `Unknown command` |

Player arguments reach Lua as values through `Sandbox::execute_with_args`
(the chunk's `...`), never spliced into source text.

## Object System

### Objects and Path IDs

An object (`objects/object.rs`) has an `id`, `universe_id`, `class`,
`parent_id` (its container), `properties` (a JSON map), an optional
`code_hash` and an optional `owner_id`. Object IDs are paths such as
`/rooms/dark-cave` (`objects/path.rs`): they start with `/`, are at most 255
characters, and each segment starts with a letter followed by `[a-z0-9-]`.
Paths are normalized to lowercase. Paths are also the unit of permission:
path grants give builders a prefix such as `/d/forest`
([permissions.md](permissions.md)).

Containment is the `parent_id` chain: rooms contain players and items,
players contain their inventory.

### Classes

`ClassRegistry` (`objects/class.rs`) holds class definitions in memory. Each
class has an optional parent, default properties, handler names and an
optional `code_hash`. Built-in classes are registered at startup and cannot be
redefined:

```
thing (name, description)
├── item (weight, value, fixed)
│   ├── weapon (damage_dice, damage_bonus, damage_type)
│   ├── armor (armor_value, slot)
│   └── container (capacity, locked)
├── living (hp, max_hp, attack_bonus, armor_class, in_combat)
│   ├── player (access_level)
│   └── npc (aggro, respawn_time)
├── room (exits, lighting, region_id)
└── region (environment_type, danger_level, ambient_sounds)
```

- **Property resolution**: `resolve_properties` walks the chain from root to
  child, children overriding parents. `game.create_object` starts a new
  object from its class's resolved defaults, minus empty-string values and
  any `code_hash` default; the props passed in override them. Class code is
  used only by `parent()`, never attached to instances.
- **Queries**: `game.is_a`, `game.get_class`, `game.get_class_chain`.
- **Custom classes**: `game.define_class(name, {parent=..., properties=...})`
  requires wizard+ and a non-built-in name. The registry is shared by every
  universe, so an existing custom class may be redefined only by the account
  that defined it (`ClassDef::defined_by`) or an admin+.
- **`parent(class, handler, args)`**: a global Lua function that runs the
  named handler from the parent class's attached code.

### Code and Handlers

Lua source is stored content-addressed in `code_store` (SHA-256 hash, so
identical code is stored once and stored code never changes).
`game.store_code` (wizard+) returns a hash. Passing `code_hash` to
`game.create_object` or `game.update_object` attaches stored code; unknown
hashes are rejected, and `nil` detaches. Only the object's owner, or someone
who outranks the owner, may change an object's `code_hash`.

Attached code must evaluate to a table of handler functions. Each handler
receives one argument table.

| Handler | Fired by | Extra arguments |
|---------|----------|-----------------|
| `on_enter` | Room entered by moving, `goto`, or spawning on connect | `from`, `direction` |
| `on_leave` | Room left by moving or `goto` | `to`, `direction` |
| `on_damage` | Target hit by `attack` | `attacker_id`, `damage` |
| `on_death` | Target slain by `attack` | `killer_id` |
| custom verb method | Verb registered with `game.add_action` | `verb`, `args` |
| timer method | `call_out` or heartbeat | `args` |
| `on_use` etc. | `game.use_object(obj, actor, verb, target)` (`use`→`on_use`, `hit`→`on_hit`, `look`→`on_look`, `init`→`on_init`, others as named) | `verb`, `code_hash`, `target_id` |

Every hook also gets `object_id`, `actor_id` and (when known) `room_id`.
Errors in hooks are logged. Lua `game.move_object` queues a
`GameMessage::Moved`; delivering it moves a connected player's session
(sending the new room) and runs the old and new containers' `on_leave` and
`on_enter` with `moved_id` and `moved_by` (no `actor_id`: no player chose
to move). A `HookBudget` carried through delivery stops move hooks past 4
nesting levels or 32 hooks per top-level run. The moved player's room view
is built without an account, so it never starts image generation.

### Custom Verbs

`game.add_action` registers a verb for a room or an object in the `actions`
table (through Raft); `ActionRegistry` caches it for dispatch. Rows cascade
with their object and their room. Because the handler runs as the object's owner,
`add_action` and `remove_action` require Modify access to the object and
being its owner or outranking the owner (`can_act_as`; ownerless objects need
only Modify), and `add_action` refuses private methods (`_name` and engine
hooks, `lua::is_private_handler`). Dispatch skips private methods too. An unknown
command looks for candidates in this order: room actions for the player's
room (only if their object is nearby), then object actions on inventory
items, objects in the room, and the room itself. If the player named a target
("pull lever"), the candidate whose object name matches wins; otherwise the
first one does. A non-empty string result becomes the reply (`nil` or `""`
sends none). Handler errors show
players "Something went wrong." while wizards+ see the details.

## Script Runner

`api/scripting.rs` is the one path through which Lua runs for commands,
`eval`, `run_script`, hooks, verbs and timers.

For every execution it:

1. Loads the universe's libraries (`lua/libs.rs`): the universe config's
   `lib_hashes` map (name to code hash), sorted by name. Missing libraries
   are logged and skipped.
2. Creates a fresh `MessageQueue` and a fresh `Sandbox`.
3. Builds a `GameApi` and sets the user, room and object contexts **before**
   registering the API, since each `game.*` closure captures them at
   registration.
4. Runs each library, then the script or handler, inside a scope that drops
   the sandbox (the Lua state is not `Send`).
5. Delivers the queued `game.send`, `game.broadcast` (with optional excluded
   player) and `game.broadcast_region` messages. A region broadcast reaches
   players whose room path is under the region's path or whose room's parent
   is the region object.

### Who Code Runs As

- `eval`, the take/drop/inventory commands: the player's account.
- `run_script`: the caller, or a lower-level account an admin names.
- Hooks, verbs, timers and `game.use_object`: **the object's owner**
  (LPC-style effective user), never the triggering player, who is only data
  (`actor_id`). Otherwise a builder could plant code that borrows the
  privileges of whoever walks in. Ownerless objects run with no user, so
  permission-checked writes fail.
- `game.use_object` runs the handler in a separate sandbox as that object's
  owner, sharing the caller's message queue and returning plain data.
  Nesting is capped at 8 levels, and one top-level execution may make at
  most 32 `use_object` calls in total (a counter shared by all nested
  sandboxes). Private handlers (`_name`) and engine hooks (`on_enter`,
  `on_leave`, `on_damage`, `on_death`, `heart_beat`, `on_create`,
  `on_destroy`) are never callable this way; such calls return `nil`. The
  `actor_id` argument is caller-supplied, so handlers must not trust it.
- `game.set_actor(id)` overrides the acting account for the rest of the run,
  but only for accounts strictly below the real user's level. The override
  lives in Rust-side app data, so Lua cannot forge it.

### Universe Isolation

Object ids are scoped per universe: `objects` is keyed `(universe_id, id)`,
and a container must be in the same universe (composite foreign key). Every
`ObjectStore` method takes the universe, and a script only ever passes its
own, so Lua reads and writes cannot reach another universe's objects. The
same `(universe, id)` keying applies to the in-memory state: sessions
(`ConnectionManager`), heartbeats and call_out caps (`TimerManager`),
contextual verbs (`ActionRegistry`) and Lua-defined classes (`ClassRegistry`; the built-in classes are global).

- An account has a separate player object, `/players/p-<account>`, with its
  own inventory and HP, in each universe it visits, and may be connected to
  several universes at once.
- Script messages (`game.send`, `game.broadcast`, `game.broadcast_region`)
  only reach sessions in the script's universe.
- Scripts cannot create player-class objects or anything under `/players/`.

### Timer Loop

`run_timer_loop` ticks every 100 ms. Each due `call_out` or heartbeat runs
its method on the object's code (as the owner) with `{object_id, room_id,
args}`. A heartbeat whose object no longer exists is removed. One-shot timers
are persisted in `timers` and heartbeats in `heartbeats` (one write to set or
remove; beats write nothing), both reloaded whenever the node becomes
primary (it alone runs this loop). The timer loop is
deadline-driven: it sleeps until the earliest `fire_at` or next beat, and a
new timer or heartbeat wakes it. An object may have at most 32 pending timers; heartbeat intervals are
at least 100 ms. `game.delete_object` cancels the object's timers and
heartbeat and clears its actions.

## Lua Sandbox

`Sandbox::new` (`lua/sandbox.rs`) creates a Lua 5.4 state with only the
`string`, `table`, `math` and `utf8` libraries, then removes `os`, `io`,
`load`, `loadstring`, `loadfile`, `dofile`, `require`, `package`, `debug` and
`collectgarbage`, and replaces `print` with a no-op.

| Limit | Object code (`SandboxConfig::default`) | `eval`, `run_script`, mudlib commands (`eval_config`) |
|-------|------|------|
| Instructions | 1,000,000 | 10,000,000 |
| Memory | 64 MB | 64 MB |
| Wall time | 500 ms | 5 s |
| DB-backed `game.*` calls | 100 | 10,000 |
| Venice calls | 5 | 5 |

- A hook every 1,000 instructions counts instructions and checks the
  wall-clock deadline; exceeding either raises an error (and again at every
  later check), and the sandbox's `pcall`/`xpcall` re-raise it.
- Memory is enforced by `Lua::set_memory_limit`.
- One budget (instructions, deadline, calls) starts when the sandbox is
  created, and a sandbox is created per execution, so libraries, object
  code and handler share it (`Sandbox::begin_execution` restarts it).
- The call budget lives in the Lua state's app data. Every `game.*` function backed by the object store,
  permissions, credits, timers or classes charges one DB call;
  `llm_chat`/`llm_image` charge a Venice call. Exceeding a budget raises a
  Lua error.

### Async Bridge

Lua callbacks are synchronous, and the game API reaches async managers in two
ways:

- `tokio::task::block_in_place` plus `Handle::current().block_on` for most
  object store and class registry calls.
- `std::thread::spawn` with a fresh Tokio runtime (`block_on_perm` and
  similar) for permission, credit, timer and Venice calls.

### Game API

The full reference is [langref.md](langref.md). The functions that exist:

| Area | Functions |
|------|-----------|
| Objects | `create_object`, `get_object`, `update_object`, `delete_object`, `move_object`, `clone_object`, `use_object`, `store_code`, `get_code` |
| Queries | `environment`, `all_inventory`, `get_children`, `present`, `get_living_in` |
| Classes | `define_class`, `get_class`, `get_class_chain`, `is_a` (plus global `parent`) |
| Verbs | `add_action`, `remove_action` |
| Messages | `send`, `broadcast`, `broadcast_region` |
| Permissions | `set_actor`, `get_actor`, `check_permission`, `can_access_path`, `get_access_level`, `set_access_level`, `grant_path`, `revoke_path`, `get_path_grants` |
| Timers | `call_out`, `remove_call_out`, `set_heart_beat`, `remove_heart_beat` |
| Status effects | `apply_effect`, `remove_effect`, `get_effects` |
| Credits | `get_credits`, `object_credits`, `request_payment`, `deduct_credits`, `transfer_credits`, `admin_grant_credits` |
| AI | `llm_chat`, `llm_image` |
| Universe | `get_universe`, `update_universe` |
| Utility | `time`, `set_time`, `set_rng_seed`, `roll_dice` |

Writes are permission-checked: `create_object` checks create permission for
the path; `update_object`, `delete_object`, `move_object` and `clone_object`
check the acting user against the object through `PermissionManager`.
`store_code` and `define_class` need wizard+; `set_time` and `set_rng_seed`
need wizard+; privileged functions such as `set_access_level`,
`admin_grant_credits` and `update_universe` enforce their own levels
([permissions.md](permissions.md)).

## Permissions

Access levels (player < builder < wizard < admin < owner), path grants and
ownership rules are described in [permissions.md](permissions.md).
Levels are per universe: `PermissionManager::get_access_level(universe,
account)` combines the member level (`universe_members`), a global admin or
owner level, and universe ownership. It caches them and loads path grants at
startup. WebSocket commands look the level up for every command, so a change
applies to open sessions at once.

## Combat

Two layers exist:

- **Rust** (`src/combat/engine.rs`): HP and combat stats have one home, the
  combatant's object row: properties `hp`, `max_hp`, `armor_class`
  (default 10), `attack_bonus` (default 0) and `resistances` (e.g.
  `{fire = "resistant"}`), the same ones Lua reads and writes. Values are
  clamped when read (HP to 0..1,000,000, armor class and attack bonus to
  ±1,000) and damage math saturates, so builder-set extremes cannot wrap. The `attack`
  command resolves a round with the pure `resolve_attack` (d20 + attack
  bonus against armor class, natural 20 a critical hit, natural 1 a fumble,
  then 1d6 physical damage, doubled on a critical, then resistances) and
  commits it as **one atomic batch** (`attack_statements`): the target's new
  `hp` via `json_set`, and on a kill its removal from the room. Targets must
  be `npc`, a class derived from `npc`, or `monster`. Effects change the
  round (`engine::attack_mods`): a stunned or paralyzed attacker cannot act,
  blinded/invisible are -4 to hit, a helpless target +4, and strengthened,
  weakened, exposed and protected adjust damage; a critical that does not
  kill inflicts bleeding, upserted in the round's batch. The attack holds the
  effect write lock and re-reads the target under it, so it never computes
  HP from a stale row.
- **Status effects** (`src/combat/effects.rs`, `src/api/effect_scheduler.rs`)
  live in `active_effects`, cached by `EffectRegistry`. They store absolute
  deadlines (`expires_at`; periodic ones also `period_ms`, `next_pulse_at`),
  computed on the primary before proposing. A deadline-driven scheduler
  sleeps until the earliest deadline (or until effects change) and commits
  everything due as one batch: pulses as HP changes, moved pulse clocks,
  ended effects and deaths. A plain effect costs one write to apply and one
  to end; a periodic effect one per pulse; nothing runs while nothing is
  due. Missed pulses (lag, downtime) are applied together, computed
  arithmetically (`engine::process_due`, a pure function). The scheduler
  reloads each due entity's rows from the table before processing (the
  cache only says when to wake), so a deleted or recreated object never
  receives stale effects; pulse periods are at least 500 ms. A player killed
  by effects drops their inventory and respawns in the same batch (HP
  restored, moved; with no workroom or portal yet, revived where they fell),
  so nobody is left dead at 0 HP; reports, including an NPC's `on_death` hook, run after
  the commit with the effect lock released.
- **Lua mudlib** (`mudd/lib/combat.lua`): `Combat.initiate`, `Combat.attack`,
  `Combat.deal_damage`, `Combat.heal`, effects, working on object
  properties. `Commands.flee` lives in `lib/commands.lua`.

Rust combat types:

- **Damage types**: physical, slashing, piercing, bludgeoning, fire, cold,
  lightning, acid, poison, necrotic, radiant, psychic, force, thunder.
- **Modifiers**: immune (0x), resistant (half, rounded down), normal,
  vulnerable (2x).
- **Status effects**: poisoned, stunned, blinded, burning, frozen, paralyzed,
  slowed, hasted, strengthened, weakened, protected, exposed, invisible,
  regenerating, silenced.
- **PvP policy** (`PvpPolicy`): `Disabled` (default), `ArenaOnly`,
  `Flagged`, `Open`. Not yet enforced by any command.
- **Dice**: `combat/dice.rs` parses `NdM+K`; Lua uses `game.roll_dice`.

## Credits

Balances live only in the `credits` table, held by an account or an object,
with no cache: `CreditManager` reads the row and writes relative updates
(`balance = balance - ?`) through Raft. `CHECK (balance >= 0)` rejects an
overdraft, so `deduct` and `transfer` (debit and credit in one batch) are
atomic and report "not enough" instead of overdrawing; no read-modify-write,
so concurrent writers (or nodes) cannot lose updates. A failed read is an
error, never a zero balance. Lua spends from the
running object (`"object"`) freely; a player's credits move only with
consent: `game.request_payment` (allowed only with a paying player, i.e. a
verb the player typed: `ScriptContext.payer`) records a `payment_requests`
row with a one-minute deadline and sends a `confirm` prompt; accepting
claims the row through Raft (one answer counts), transfers the credits to
the object and runs its handler (refunding if it fails), in
`api/payments.rs`. Passive
code (hooks, timers, nested `use_object`) has no payer, so walking into a
room can never cost a player anything. Lua exposes `get_credits`,
`object_credits`, `deduct_credits`, `transfer_credits` and
`admin_grant_credits`. The engine charges nothing itself, and purchasing is out of
scope (see [docs/blockchain.md](../../docs/blockchain.md)).

## Venice AI and Room Images

`VeniceClient` (`src/venice/`) talks to an OpenAI-compatible API, configured
by `VENICE_API_KEY` and `VENICE_API_URL` ([admin.md](admin.md)). Without a
key, AI features are off. Chat tiers are fast, balanced and quality. A
token-bucket rate limiter allows 60 requests per minute per key. Lua calls
cost the universe config's `venice_chat_cost` / `venice_image_cost` credits
(default 0), charged to the payer (`"actor"` or `"object"`) before the
call and refunded if it fails; the rate limit is kept per universe and
code owner, whoever pays. Room images are the operator's cost and charge nothing.

Room images (`images/gen.rs`) use two steps:

1. A fast-tier chat call turns the theme's visual style plus the room's name,
   description and region environment type into an image prompt (no people,
   no text).
2. An image call renders it; the PNG is stored in `image_store` by SHA-256
   hash and the room's `image_hash` and `image_generated_at` properties are
   set.

`build_room_message` starts this in a background task when a room without
`image_hash` is shown to an authenticated player and Venice is configured. One job
runs per room at a time (`ImageJobs`; its guard frees the room however the
job ends), and the job re-checks the room before paying for anything. A
room needs an image only when `image_hash` is missing or null
(`room_needs_image`). The result is written in place (`image_hash`, and
`image_generated_at` as a Unix timestamp, only, so changes made during
generation survive) and only if the room still has no image.
The client fetches `/images/{hash}`. The theme is currently always
`sierra-retro` (`ThemeRegistry` also has `modern`); its style is the Sierra
adventure-game look (King's Quest era, 320x200 VGA, 256 colors, dithered,
3/4 elevated view).

## Raft Replication

`openraft` 0.10 with storage in the same SQLite database (`raft/storage.rs`:
`LogStore` for the log and vote, `StateMachine` for applying entries):

- **Log entries** are SQL requests (`raft/types.rs`: `Request { statements }`,
  each statement SQL plus bound parameters), applied in one transaction: all
  or nothing. Managers write through `RaftWriter::execute` (one statement) or
  `execute_batch` (several, atomically); non-deterministic values (UUIDs,
  timestamps) are computed before proposing. Reads go straight to the pool.
- **Runs** (`db/run.rs`): a Lua run (`scripting::in_run`) is one change.
  Its writes execute at once on the run's own connection inside a local
  SQLite transaction (so its reads, through `db::run::reader`, see them)
  and are captured; when the run succeeds the local transaction is rolled
  back and the captured statements are proposed as one entry. A failed run
  proposes nothing (except `execute_kept` statements, e.g. a paid Venice
  call's charge) and rebuilds the caches from the tables
  (`primary::rebuild_caches`), since managers update their caches as writes
  happen. A process-wide write gate, held from a run's first write until
  its outcome is settled, makes writing runs take turns, so that rebuild
  cannot race another run's pending change.
- **Batching**: openraft merges concurrent client writes into one log
  append, and the state machine applies each batch it is handed as one
  SQLite transaction (each request in its own savepoint, so requests stay
  independent), writing `last_applied` in that transaction, and answers
  the clients after it commits. An entry is therefore applied exactly once,
  even across a crash. A request whose SQL fails gets an error response;
  a storage failure while applying (the transaction itself failing, e.g. a
  full disk) is fatal to the Raft core, and `mudd` then stops serving and
  exits non-zero (`RaftWriter::stopped`) so a supervisor can restart it.
- **Every server write goes through Raft**, including accounts and combat
  state. Only `mudd_init` (and `AccountService::offline`) writes directly,
  because it runs before any Raft node exists.
- **Persistence**: `raft_log` (entries, with term and `leader_node`),
  `raft_vote` (vote and committed index), `raft_meta` (`last_applied`,
  `last_purged`, `last_membership`). Persisting the membership lets a
  restarted single node elect itself again.
- **Snapshots**: after 1,000 log entries, so a single node can purge its
  log. A snapshot is a consistent copy of the game tables (`VACUUM INTO` a
  temp file, which reads in one transaction while writes go on; the Raft
  tables are dropped from the copy) with the applied position and
  membership recorded inside that copy, plus its schema version. Installing
  one (same schema version only) attaches the copy and replaces every game
  table's rows in one transaction, with foreign keys deferred, then records
  the position: the node has the old state or the new, its own Raft log and
  vote stay, and no connection the application holds is closed.
- **One durable commit per write** (as in roodb): the log append syncs;
  applying does not (`synchronous = NORMAL` on that connection, restored to
  `FULL` before it goes back to the pool). A crash may roll back an apply,
  but its `last_applied` goes with it (same transaction), so the entry is
  applied again from the log, exactly once. Building a snapshot first makes
  a syncing commit, so the log is never purged past data not yet on disk.
  The committed log position is recorded as a lower bound
  (`save_committed`, not synced). About 2.2 ms per write instead of 3.9.
- **Timing**: heartbeat 100 ms, election timeout 400-800 ms. Timers start
  off and are switched on at runtime once the membership has more than one
  node.
- **Transport** (`raft/transport.rs`, after the sister project roodb): one
  TLS listener per node; frames `[kind][len][JSON body]` (JSON because log
  entries carry `serde_json::Value` parameters), each sent as one write
  with `TCP_NODELAY`; one kept connection per peer, re-made and the request
  repeated once when it turns out dead; every RPC under openraft's
  `soft_ttl`; snapshots streamed on their own connection in 1 MiB chunks.
  The server is verified against the cluster CA as `mudd-cluster`, then the
  client's `Hello` must carry the shared secret (constant-time compare).
- **Membership**: a new node (`mudd_init --empty`, `mudd --join`) asks any
  member; one that is not the leader names the leader. The leader assigns
  the next id, snapshots and purges its log (node 1's `mudd_init` data
  predates the log, so a newcomer must start from a snapshot), adds the
  node as a learner (waiting for it to catch up) and promotes it to voter.
  As leader, a node records its own advertised address in the membership if
  it changed (e.g. a single-node database given cluster flags).

- **Gateways** (`api/gateway.rs`): on a node that is not the primary,
  `/auth/*` and `/universe/*` go through middleware that forwards the
  request over the cluster port (`Http` frame: method, path, the
  `authorization` and `content-type` headers, base64 body) to the primary,
  which runs it through its own router (marked `x-mudd-forwarded`, so a node
  that is no longer primary answers 503 instead of forwarding again). `/ws`
  checks the token with the primary (this node's copy may lack a
  just-registered account), then relays the connection to a session on the
  primary over a dedicated cluster connection (`GameOpen`, then text
  frames). The primary runs that session exactly like a local one
  (`SessionIo::Remote`) and ends it on purpose with an end frame; a
  connection that drops without one is a failover, and the gateway tells
  the player and reopens the session on whichever node is primary now
  (retrying for up to 30 s). A primary that steps down closes its sessions
  without the end frame, so they move too.
- **Leadership** is the server state being Leader (a leader that steps down
  can still name itself as `current_leader`). A removed leader hands over
  with `transfer_leader`.

Known gaps: [docs/roadmap.md](../../docs/roadmap.md).

## Key Design Decisions

1. **Content-addressed code**: Lua source is stored by SHA-256, deduplicated
   and immutable.
2. **Sandbox per execution**: every run gets a fresh Lua state and message
   queue, so nothing leaks between executions.
3. **Object code runs as its owner**: the triggering player is data, never
   identity.
4. **Values, not source**: untrusted input reaches Lua as arguments
   (`execute_with_args`), never as code.
5. **Non-blocking delivery**: bounded per-client queues with `try_send`.
6. **SQL-level replication**: Raft replicates the SQL a write performs, so
   followers never run Lua.
7. **Timers**: one-shot timers and heartbeats survive restart; registered
   verbs are in memory and must be re-registered.
8. **WebSocket JSON**: messages tagged by a `type` field.
