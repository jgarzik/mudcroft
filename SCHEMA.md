# Mudcroft Database Schema

Mudcroft keeps all state in a single SQLite database (WAL mode, foreign keys
on). `mudd_init` creates it, and re-running `mudd_init` upgrades it in place:
`mudd_init` is the only place schema upgrades live (`mudd` never migrates).
The schema version is kept in `PRAGMA user_version` (`SCHEMA_VERSION`, now 13):
`mudd` refuses a database at any other version, and `mudd_init` upgrades an
older one step by step (`Database::upgrade`). The upgrade steps are
6 -> 7 (adds the `credits_holder_exists` trigger), 7 -> 8 (adds the
`actions` table), 8 -> 9 (adds `cluster_nodes`), 9 -> 10 (adds `online_players`) and
10 -> 11 (adds `payment_requests`), 11 -> 12 (adds `applied_commands`), 12 -> 13 (adds `combat` and `deadlines`); an older database is refused with a "create a new database" error (from
version 1 there is none: its SHA-256 password hashes cannot become argon2id);
at the current version a re-run creates any missing tables and refreshes the
core mudlib.
The schema is defined in `mudd/src/db/mod.rs` (`run_migrations`). This file
mirrors it.

All tables are shared by every universe. Rows are scoped by a `universe_id`
column where one exists.

Conventions used below:

- **Universe ids** are DNS-style: 3-64 characters, lowercase letters, digits
  and hyphens. They start and end with an alphanumeric character and contain
  no `--`. Examples: `my-game`, `test-world`.
- **Object ids** are lowercase paths, e.g. `/rooms/cave-entrance`. Every
  segment starts with a letter and contains only `[a-z0-9-]`. The maximum
  length is 255 characters. Object ids are scoped per universe: the primary
  key of `objects` is `(universe_id, id)`, so two universes can both have
  `/rooms/start`. An account has one player object in each universe it
  visits, `/players/p-<account_id>`.
- **Account ids**, and the ids of credits, timers, grants and effects, are
  UUIDs.
- Timestamps are text from SQLite `datetime('now')` (`YYYY-MM-DD HH:MM:SS`,
  UTC) unless noted. Code that writes its own timestamps uses RFC 3339.

## Accounts and Universes

### accounts

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| username | TEXT UNIQUE NOT NULL | Login name (case-sensitive) |
| password_hash | TEXT | argon2id PHC string (`$argon2id$v=19$...`, default parameters, random salt) |
| token | TEXT | Current session token: 64 hex characters, opaque, not a JWT. `NULL` after logout. A new login replaces it. |
| access_level | TEXT NOT NULL DEFAULT 'player' | Global level. Only `admin` and `owner` have an effect (in every universe); per-universe levels are in `universe_members`. See [permissions.md](mudd/docs/permissions.md#access-levels). |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

`mudd` refuses to start unless at least one row has `access_level` `admin` or
`owner`.

### universes

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | DNS-style universe id |
| name | TEXT NOT NULL | Display name |
| owner_id | TEXT NOT NULL REFERENCES accounts(id) | |
| config | TEXT NOT NULL DEFAULT '{}' | JSON. The server sets `lib_hashes` (library name to `code_store` hash) at creation. Other keys are free-form. |
| theme_id | TEXT NOT NULL DEFAULT 'sierra-retro' | Not yet read: the server always sends `sierra-retro` |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

### universe_members

Per-universe access levels. No row means `player`. Owner is never stored
here: it comes from `universes.owner_id` or a global owner account.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| account_id | TEXT NOT NULL REFERENCES accounts(id) | |
| level | TEXT NOT NULL | `player`, `builder`, `wizard` or `admin` |

Primary key: `(universe_id, account_id)`.

### universe_settings

Key-value settings per universe.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| key | TEXT NOT NULL | |
| value | TEXT NOT NULL | |

Primary key: `(universe_id, key)`.

Keys in use: `portal_room_id`, the object id of the spawn room (set by `setportal`).

## Objects and Code

### objects

Rooms, items, NPCs, players: every game object.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| id | TEXT NOT NULL | Object path, unique within the universe |
| class | TEXT NOT NULL | Class name, e.g. `room`, `item`, `npc`, `player` |
| parent_id | TEXT | Container: the room an item or player is in, or the player carrying an item. `NULL` = nowhere. Foreign key `(universe_id, parent_id)` to `objects(universe_id, id)`, so a container is always in the same universe. |
| properties | TEXT NOT NULL DEFAULT '{}' | JSON object (`name`, `description`, `exits`, `fixed`, `hp`, ...) |
| code_hash | TEXT | `code_store.hash` of the object's attached Lua code. No foreign key. |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |
| updated_at | TEXT NOT NULL DEFAULT (datetime('now')) | |
| owner_id | TEXT REFERENCES accounts(id) | Creator. Object code runs as this account. |

Primary key: `(universe_id, id)`. Indexes: `idx_objects_universe (universe_id)`,
`idx_objects_parent (universe_id, parent_id)`, `idx_objects_owner (owner_id)`.

### code_store

Content-addressed Lua source: universe libraries, object code and stored snippets.

| Column | Type | Notes |
|--------|------|-------|
| hash | TEXT PRIMARY KEY | Hex SHA-256 of `source` |
| source | TEXT NOT NULL | |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

### core_lib_hashes

The core mudlib version loaded by `mudd_init` from `lib/` (or `--lib-dir`).
A new universe copies these entries into its `config.lib_hashes`, so existing
universes keep the library versions they were created with.

| Column | Type | Notes |
|--------|------|-------|
| name | TEXT PRIMARY KEY | Library name (file stem, e.g. `commands`) |
| hash | TEXT NOT NULL | `code_store.hash`. No foreign key. |
| updated_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

### image_store

Content-addressed images, served at `GET /images/{hash}`.

| Column | Type | Notes |
|--------|------|-------|
| hash | TEXT PRIMARY KEY | Hex SHA-256 of `data` |
| data | BLOB NOT NULL | |
| mime_type | TEXT NOT NULL | |
| size_bytes | INTEGER NOT NULL | |
| source | TEXT | Where the image came from |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |
| reference_count | INTEGER DEFAULT 0 | |

## Classes

Classes defined with `game.define_class`, per universe. The built-in base
classes are compiled in and never stored; a stored class may name one as its
parent. `define_class` writes the three tables in one atomic Raft batch.

### classes

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| name | TEXT NOT NULL | |
| parent | TEXT | Built-in or same-universe class. No foreign key. |
| code_hash | TEXT | No foreign key |
| defined_by | TEXT REFERENCES accounts(id) | Only this account (or an admin+) may redefine the class |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

Primary key: `(universe_id, name)`.

### class_properties

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| class_name | TEXT NOT NULL | |
| key | TEXT NOT NULL | |
| value | TEXT NOT NULL | JSON-encoded default |

Primary key: `(universe_id, class_name, key)`. Foreign key
`(universe_id, class_name)` to `classes(universe_id, name)` ON DELETE CASCADE.

### class_handlers

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| class_name | TEXT NOT NULL | |
| handler | TEXT NOT NULL | Handler name, e.g. `on_init` |

Primary key: `(universe_id, class_name, handler)`. Foreign key
`(universe_id, class_name)` to `classes(universe_id, name)` ON DELETE CASCADE.

## Permissions

### path_grants

Delegated access to an object-path prefix. See
[permissions.md](mudd/docs/permissions.md#path-grants).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| grantee_id | TEXT NOT NULL REFERENCES accounts(id) | |
| path_prefix | TEXT NOT NULL | e.g. `/d/forest`. Matches on `/` boundaries. |
| can_delegate | BOOLEAN NOT NULL DEFAULT FALSE | |
| granted_by | TEXT NOT NULL REFERENCES accounts(id) | |
| granted_at | TEXT NOT NULL | RFC 3339 |

Unique: `(universe_id, grantee_id, path_prefix)`. Index:
`idx_path_grants_grantee (universe_id, grantee_id)`.

## Economy

### credits

Per-universe balances, held by an account or an object (a room's budget, a
shop's till). The only copy of a balance: every change is a relative update
in one atomic batch, and the CHECK rejects an overdraft, so a spend or a
transfer is all-or-nothing.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| holder_kind | TEXT NOT NULL | `account` or `object` |
| holder_id | TEXT NOT NULL | Account id, or object path in the universe |
| balance | INTEGER NOT NULL DEFAULT 0 | `CHECK (balance >= 0)` |

Primary key: `(universe_id, holder_kind, holder_id)`. No foreign key (a holder
may be an account or an object); instead the trigger `credits_holder_exists`
aborts an insert whose holder does not exist (an unknown account, or no such
object in the universe), in the same write. Deleting an object deletes its
balance in the same batch.

## Combat

### active_effects

Status effects (poison, stun, bleeding, ...): the source of truth, which
`EffectRegistry` caches. One row per effect type on an entity; applying the
same type again keeps the later end and the stronger magnitude. Times are
absolute deadlines (Unix ms), so a plain effect is never rewritten while it
lasts and a periodic one only once per pulse.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| entity_id | TEXT NOT NULL | Object id in that universe |
| effect_type | TEXT NOT NULL | `poisoned`, `stunned`, `bleeding`, ... |
| expires_at | INTEGER NOT NULL | When the effect ends (Unix ms) |
| period_ms | INTEGER | Periodic effects: milliseconds between pulses; NULL otherwise |
| next_pulse_at | INTEGER | Periodic effects: when the next pulse is due (Unix ms) |
| magnitude | INTEGER NOT NULL DEFAULT 0 | Damage or healing per pulse, or strength of a buff/debuff |
| damage_type | TEXT | For damage-over-time effects |
| source_id | TEXT | Who applied it |

Primary key: `(universe_id, entity_id, effect_type)`. Foreign key
`(universe_id, entity_id)` to `objects(universe_id, id)` ON DELETE CASCADE:
deleting an object deletes its effects.

## Timers

### timers

Pending one-shot `game.call_out` timers.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| object_id | TEXT NOT NULL | Object whose handler is called |
| method | TEXT NOT NULL | Handler name |
| fire_at | INTEGER NOT NULL | Unix time (ms) |
| args | TEXT | JSON |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

Foreign key `(universe_id, object_id)` to `objects(universe_id, id)` ON DELETE
CASCADE. Index: `idx_timers_fire_at (fire_at)`.

### heartbeats

Recurring `game.set_heart_beat` heartbeats, one per object. Setting or
removing one is a write; a beat is not (after a restart the next beat is one
interval away).

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| object_id | TEXT NOT NULL | Object whose `heart_beat` handler is called |
| interval_ms | INTEGER NOT NULL | At least 100 |

Primary key: `(universe_id, object_id)`. Foreign key to `objects` ON DELETE
CASCADE.

## Raft

Used by the OpenRaft log store. Game writes go through Raft and are applied to
the tables above.


### actions

Custom verbs (`game.add_action`). The dispatch cache (`ActionRegistry`) is
rebuilt from this table when a node becomes primary.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| scope | TEXT NOT NULL | `room` (the verb works in that room) or `object` (it follows the object) |
| scope_id | TEXT NOT NULL | The room, or the object itself |
| verb | TEXT NOT NULL | |
| object_id | TEXT NOT NULL | The object whose code handles it |
| method | TEXT NOT NULL | The handler's name |

Primary key: `(universe_id, scope, scope_id, verb, object_id)`. Foreign keys
`(universe_id, object_id)` and `(universe_id, scope_id)` to `objects` ON
DELETE CASCADE: a verb goes with its object and with its room.
### raft_log

| Column | Type | Notes |
|--------|------|-------|
| log_index | INTEGER PRIMARY KEY | |
| term | INTEGER NOT NULL | |
| leader_node | INTEGER NOT NULL DEFAULT 0 | Node id of the leader that created the entry. Older databases get it via `ALTER TABLE` when the Raft store opens. |
| entry_type | TEXT NOT NULL | |
| payload | TEXT | JSON |
| created_at | INTEGER NOT NULL DEFAULT (unixepoch()) | Unix time |

Index: `idx_raft_log_term (term)`.

### raft_vote

A single row holding the current vote.

| Column | Type | Notes |
|--------|------|-------|
| id | INTEGER PRIMARY KEY CHECK (id = 1) | Always 1 |
| term | INTEGER NOT NULL | |
| node_id | INTEGER | Node voted for |
| committed | INTEGER NOT NULL DEFAULT 0 | 0 or 1 |

### raft_meta

| Column | Type | Notes |
|--------|------|-------|
| key | TEXT PRIMARY KEY | |
| value | TEXT NOT NULL | JSON |

Keys:

| Key | Value |
|-----|-------|
| `last_applied` | Last log id applied to the state machine |
| `last_purged` | Last log id removed from `raft_log` by log compaction |
| `last_membership` | Last applied cluster membership (also written on snapshot install). Reloaded at startup so a restarted node still has its voters. |


### cluster_nodes

Every node ever admitted to the cluster, written by the leader through Raft
before it answers a join (so it travels in snapshots and survives
failover). New ids are above every id recorded here: a removed node's id is
never reused, so it cannot come back as a newer member.

| Column | Type | Notes |
|--------|------|-------|
| node_id | INTEGER PRIMARY KEY | |
| addr | TEXT NOT NULL | The cluster address it joined with |
| added_at | TEXT NOT NULL | RFC 3339 |

### online_players

Players with a game session: written when a session starts, removed when
the player's disconnect is handled (after the reconnect grace period). When
a node becomes primary it waits out that grace period and handles the
disconnect of everyone listed who has not reconnected, so a player whose
server crashed or stepped down is still cleaned up.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| player_id | TEXT NOT NULL | The player object |
| since | TEXT NOT NULL | RFC 3339 |

Primary key `(universe_id, player_id)`; foreign key to `objects` ON DELETE
CASCADE.

### payment_requests

Payments a player has been asked to confirm (`game.request_payment`). A row
is deleted when it is answered (through Raft, so only one answer counts) or
found expired; the payment itself is a credits transfer from the player to
the object.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL | |
| account_id | TEXT NOT NULL | The player asked to pay |
| object_id | TEXT NOT NULL | The object asking (and paid) |
| handler | TEXT NOT NULL | Its handler to run once paid |
| amount | INTEGER NOT NULL | `CHECK (amount > 0)` |
| reason | TEXT NOT NULL | Shown to the player |
| data | TEXT NOT NULL | Passed to the handler |
| expires_at | INTEGER NOT NULL | ms since epoch (a minute after the request) |

Foreign key `(universe_id, object_id)` to `objects` ON DELETE CASCADE.

### applied_commands

Commands saved with an id (`{"type": "command", "id"}`, set by gateways):
a command sent again with a saved id (a retry after a failover) is not run
twice. The row is written in the command's own Raft entry; rows past their
deadline are deleted by later such entries.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| player_id | TEXT NOT NULL | The player who sent it (ids are per player) |
| command_id | TEXT NOT NULL | Up to 64 characters |
| expires_at | INTEGER NOT NULL | ms since epoch (ten minutes after) |

Primary key `(universe_id, player_id, command_id)`.

### combat

Fights in progress: one row per fighter (`api/combat.rs`).

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| entity_id | TEXT NOT NULL | The fighter |
| target_id | TEXT NOT NULL | Whom it attacks |
| next_round_at | INTEGER NOT NULL | ms since epoch; index `combat_due` |

Primary key `(universe_id, entity_id)`; foreign key `(universe_id, entity_id)`
to `objects` ON DELETE CASCADE.

### deadlines

World events due at an absolute time: a corpse decaying, an NPC respawning
or wandering. Triggers `wander_on_insert` and `wander_on_update` give an
object whose properties get a positive `wander_seconds` a `wander` row due at
time 0 (the same on every replica); the scheduler reschedules it.

| Column | Type | Notes |
|--------|------|-------|
| universe_id | TEXT NOT NULL | |
| object_id | TEXT NOT NULL | The corpse or NPC |
| kind | TEXT NOT NULL | `decay`, `respawn`, `wander` or `delete` (an NPC gone for good, deleted right after its death is reported) |
| due_at | INTEGER NOT NULL | ms since epoch; index `deadlines_due` |

Primary key `(universe_id, object_id, kind)`; foreign key to `objects` ON
DELETE CASCADE.

Trigger `unequip_on_move` (schema 13) on `objects`: when an object's
`parent_id` changes, its `equipped_by` property (equipment) is removed.

## Startup Validation

`mudd` opens an existing database only. It does not create the database or
run migrations. It checks that these tables exist:

`accounts`, `universes`, `objects`, `code_store`, `credits`, `classes`,
`class_properties`, `class_handlers`, `timers`, `universe_members`, `raft_log`, `raft_vote`, `raft_meta`.

It also checks that an `admin` or `owner` account exists. If either check fails, run
`mudd_init` against the database.

## Index Summary

| Index | Table (columns) |
|-------|-----------------|
| idx_objects_universe | objects(universe_id) |
| idx_objects_parent | objects(universe_id, parent_id) |
| idx_objects_owner | objects(owner_id) |
| idx_path_grants_grantee | path_grants(universe_id, grantee_id) |
| idx_timers_fire_at | timers(fire_at) |
| idx_raft_log_term | raft_log(term) |

SQLite also creates automatic indexes for the PRIMARY KEY and UNIQUE
constraints listed above.
