# Server Design Guide

## Architecture Overview

```
mudd/src/
├── main.rs          # CLI entry, server startup
├── lib.rs           # Config, Server struct
├── init.rs          # Database initialization logic
├── db/              # SQLite with WAL mode
├── api/             # HTTP/WebSocket endpoints (Axum)
│   ├── auth.rs      # /auth/* endpoints
│   ├── websocket.rs # /ws, ConnectionManager, PlayerSession
│   ├── universe.rs  # /universe/* endpoints
│   └── images.rs    # /images/* endpoints
├── auth/            # Account service, password hashing
├── objects/         # LPC-style object system
│   ├── object.rs    # Object struct, properties
│   ├── store.rs     # ObjectStore CRUD
│   └── class.rs     # ClassDef, ClassRegistry, inheritance
├── lua/             # Sandboxed Lua execution
│   ├── sandbox.rs   # Instruction/memory limits
│   ├── game_api.rs  # game.* functions exposed to Lua
│   ├── actions.rs   # ActionRegistry for verbs
│   ├── messaging.rs # MessageQueue for broadcasts
│   └── metering.rs  # Resource tracking
├── combat/          # Combat system
│   ├── dice.rs      # Dice notation parser
│   ├── damage.rs    # DamageType, modifiers
│   ├── effects.rs   # StatusEffect, EffectRegistry
│   └── state.rs     # CombatManager, PvpPolicy
├── permissions/     # Role-based access control
├── timers/          # call_out, heartbeat
├── credits/         # Economy system
├── venice/          # Venice AI client
├── images/          # Image storage/generation
├── theme/           # UI theme registry
└── raft/            # Raft consensus
    ├── storage.rs   # Log storage
    ├── state_machine.rs
    ├── network.rs   # RPC transport
    └── snapshot.rs
```

## Database Schema

### Core Tables

| Table | Purpose |
|-------|---------|
| `accounts` | User accounts with password hash, token, access_level |
| `universes` | Game worlds with config JSON |
| `objects` | Game objects with class, parent_id, properties JSON |
| `code_store` | Content-addressed Lua source (hash -> source) |
| `classes` | Custom class definitions |
| `credits` | Player credit balances per universe |
| `timers` | Persisted one-shot timers |
| `universe_settings` | Key-value settings per universe (e.g., portal_room_id) |

### Permission/Combat Tables

| Table | Purpose |
|-------|---------|
| `builder_regions` | Builder region assignments |
| `combat_state` | HP, armor_class, attack_bonus per entity |
| `active_effects` | Status effects with remaining ticks |

### Raft Tables

| Table | Purpose |
|-------|---------|
| `raft_log` | Replicated log entries |
| `raft_vote` | Current term and voted_for |
| `raft_meta` | Metadata (committed index, etc.) |

### Key Relationships

```
accounts(id) <-- universes(owner_id)
accounts(id) <-- credits(player_id)
accounts(id) <-- builder_regions(account_id)
universes(id) <-- objects(universe_id)
objects(id) <-- objects(parent_id)  -- containment hierarchy
code_store(hash) <-- objects(code_hash)
combat_state(entity_id) <-- active_effects(entity_id)
```

## Startup Sequence

1. Parse CLI args (`--database`, `--bind`)
2. Open SQLite database (validates schema + admin account)
3. Build `AppState` with managers:
   - `ConnectionManager` (WebSocket sessions)
   - `ObjectStore` (database operations)
   - `ClassRegistry` (load from DB)
   - `ActionRegistry`, `MessageQueue`
   - `PermissionManager` (load builder regions)
   - `TimerManager` (load persisted timers)
   - `CreditManager`
   - `VeniceClient`, `ImageStore`, `ThemeRegistry`
4. Bind TCP listener
5. Serve Axum router with graceful shutdown

## Request Flow

### HTTP Request

```
Client -> TcpListener -> Axum Router
       -> Extract State/Path/Query/JSON
       -> Handler function
       -> Response (JSON/StatusCode)
```

### WebSocket Connection

```
Client -> /ws?token=<jwt>
       -> ws_handler: validate token, upgrade
       -> handle_socket:
          1. Create PlayerSession (player_id, sender channel)
          2. Register with ConnectionManager
          3. Send Welcome message
          4. Spawn at portal room (if set) or send "not initialized" message
          5. Loop: recv ClientMessage -> execute_command -> send ServerMessage
       -> On disconnect: unregister session
```

### Command Execution

```
ClientMessage::Command{text}
  -> parse verb + args
  -> match verb:
     - look, north/south/east/west, say, help: built-in handlers
     - goto <room_id>: Wizard+ only, teleport to room
     - setportal [room_id]: Wizard+ only, set spawn point
     - eval: Wizard+ only, creates Sandbox with GameApi
  -> execute, send ServerMessage
```

## Core Subsystems

### Object System

**Classes** (LPC-style inheritance):
- Base: `thing` (name, description)
- `item` extends `thing` (weight, value, fixed)
- `living` extends `thing` (hp, max_hp, armor_class)
- `room` extends `thing` (exits, lighting, region_id)
- `weapon` extends `item` (damage_dice, damage_type)
- `player` extends `living`
- `npc` extends `living` (aggro, respawn_time)

**Property Resolution**: Walk inheritance chain root->child, child overrides.

**Storage**:
- Objects stored in `objects` table with properties JSON
- Code stored separately in `code_store` (content-addressed by SHA256)
- `code_hash` on object references handler code

### Lua Sandbox

**Configuration** (`SandboxConfig`):
- `max_instructions`: 1,000,000 default
- `max_memory`: 64MB default
- `timeout`: 500ms default
- `max_db_queries`: 100
- `max_venice_calls`: 5

**Restricted Globals**: Removes `os`, `io`, `load`, `loadfile`, `dofile`, `require`, `package`, `debug`, `collectgarbage`.

**Available Libraries**: `string`, `table`, `math`, `utf8`.

**Metering**: Instruction counting via hook (every 1000 instructions), memory tracking via `lua.used_memory()`.

**Game API** (`game.*`):
| Function | Description |
|----------|-------------|
| `create_object(class, parent_id, props)` | Create object in DB |
| `get_object(id)` | Fetch object |
| `update_object(id, changes)` | Update properties |
| `delete_object(id)` | Remove object |
| `move_object(id, new_parent)` | Reparent |
| `define_class(name, def)` | Register class |
| `is_a(obj_id, class)` | Check inheritance |
| `environment(obj_id)` | Get container |
| `all_inventory(obj_id)` | Get contents |
| `present(name, env_id)` | Find by name |
| `send(target_id, msg)` | Private message |
| `broadcast(room_id, msg)` | Room broadcast |
| `check_permission(action, target, is_fixed, region)` | Permission check |
| `call_out(delay, method, args)` | Schedule timer |
| `set_heart_beat(interval_ms)` | Recurring timer |
| `get_credits()` | Get balance |
| `deduct_credits(amount, reason)` | Spend credits |
| `llm_chat(messages, tier)` | Venice chat |
| `llm_image(prompt, style, size)` | Venice image |
| `roll_dice(dice_str)` | Parse and roll (e.g., "2d6+3") |
| `time()` | Current time (ms) |

### Combat System

**Damage Types**: `physical`, `fire`, `ice`, `lightning`, `poison`, `necrotic`, `radiant`, `psychic`.

**Damage Modifiers**: `Normal`, `Immune`, `Resistant` (half), `Vulnerable` (double).

**Status Effects**:
| Effect | Description |
|--------|-------------|
| Poisoned | DOT, ticks down |
| Burning | Fire DOT |
| Frozen | Movement penalty |
| Stunned | Skip turn |
| Paralyzed | Cannot act |
| Blessed | Bonus to rolls |
| Cursed | Penalty to rolls |

**Attack Resolution**:
1. Roll d20 + attack_bonus vs target armor_class
2. On hit: roll damage dice, apply modifiers
3. Update HP, check death

**PvP Policy**: `Disabled`, `Consensual`, `Enabled`.

### Permission System

**Access Levels** (ordered):
| Level | Capabilities |
|-------|--------------|
| Player | Read, interact with non-fixed objects |
| Builder | Create/modify in assigned regions |
| Wizard | Full object control, bypass fixed |
| Admin | Universe config, grant credits |
| Owner | Grant admin access |

**Region Assignment**: Builders get specific regions via `assign_region()`.

**Permission Check**: `check_permission(user, action, object)` returns `Allowed` or `Denied(reason)`.

### Timer System

**call_out** (one-shot):
- Persisted to `timers` table
- Fires after delay, calls method on object
- Returns timer_id for cancellation

**heartbeat** (recurring):
- In-memory only (not persisted)
- Calls `heart_beat` method at interval
- Used for NPC AI, effects tick

**Tick Loop**: `TimerManager::tick()` returns fired callbacks for execution.

### Raft Consensus

**Implementation**: OpenRaft library.

**Storage**: SQLite tables (`raft_log`, `raft_vote`, `raft_meta`).

**Replication Model**:
- Leader accepts writes, replicates to followers
- Committed entries applied to state machine
- Snapshots for log compaction

## Key Design Decisions

1. **Content-addressed code**: Lua source stored by SHA256 hash, enabling deduplication and immutability.

2. **Sandbox-per-execution**: Fresh Lua state for each `eval`, preventing state leakage between executions.

3. **Async-in-sync bridge**: Game API uses `block_in_place` for database calls within sync Lua context.

4. **Class registry in memory**: Base classes always available, custom classes loaded from DB on startup.

5. **WebSocket JSON protocol**: Tagged messages (`type` field) for typed client/server communication.

6. **Permissions cached**: Access levels cached in memory, backed by database for persistence.

7. **Timer persistence**: One-shot timers survive restart, heartbeats do not (re-registered on object load).
