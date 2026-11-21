# Mudcroft Database Schema

Mudcroft keeps all state in a single SQLite database (WAL mode, foreign keys
on). `mudd_init` creates it, and re-running `mudd_init` upgrades it in place.
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
  length is 255 characters. Object ids are the primary key of `objects`, so a
  path is unique across the whole database, not per universe. Player objects
  are `/players/p-<account_id>`.
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
| password_hash | TEXT | Hex SHA-256 of `salt` + password |
| salt | TEXT | 16 random bytes, hex |
| token | TEXT | Current session token: 64 hex characters, opaque, not a JWT. `NULL` after logout. A new login replaces it. |
| access_level | TEXT NOT NULL DEFAULT 'player' | `player`, `builder`, `wizard`, `admin` or `owner`. See [permissions.md](mudd/docs/permissions.md#access-levels). |
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
| id | TEXT PRIMARY KEY | Object path |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| class | TEXT NOT NULL | Class name, e.g. `room`, `item`, `npc`, `player` |
| parent_id | TEXT REFERENCES objects(id) | Container: the room an item or player is in, or the player carrying an item. `NULL` = nowhere. |
| properties | TEXT NOT NULL DEFAULT '{}' | JSON object (`name`, `description`, `exits`, `fixed`, `hp`, ...) |
| code_hash | TEXT | `code_store.hash` of the object's attached Lua code. No foreign key. |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |
| updated_at | TEXT NOT NULL DEFAULT (datetime('now')) | |
| owner_id | TEXT REFERENCES accounts(id) | Creator. Object code runs as this account. Added by `ALTER TABLE` migration, so it is the last column. |

Indexes: `idx_objects_universe (universe_id)`, `idx_objects_parent (parent_id)`,
`idx_objects_owner (owner_id)`.

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

These tables are meant to persist classes defined with `game.define_class`.
The built-in base classes are compiled in and never stored.

**Known limitation:** Lua-defined classes do not persist in practice. The
writer inserts `universe_id = ''`, which fails the foreign key to `universes`
(and a `parent` naming a built-in class fails `REFERENCES classes(name)`). A
custom class therefore lives only in memory until restart. See
[docs/roadmap.md](docs/roadmap.md).

### classes

| Column | Type | Notes |
|--------|------|-------|
| name | TEXT PRIMARY KEY | Global across universes |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| parent | TEXT REFERENCES classes(name) | |
| code_hash | TEXT | No foreign key |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

### class_properties

| Column | Type | Notes |
|--------|------|-------|
| class_name | TEXT NOT NULL REFERENCES classes(name) ON DELETE CASCADE | |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| key | TEXT NOT NULL | |
| value | TEXT NOT NULL | JSON-encoded default |

Primary key: `(class_name, key)`. Index: `idx_class_props_universe (universe_id)`.

### class_handlers

| Column | Type | Notes |
|--------|------|-------|
| class_name | TEXT NOT NULL REFERENCES classes(name) ON DELETE CASCADE | |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| handler | TEXT NOT NULL | Handler name, e.g. `on_init` |

Primary key: `(class_name, handler)`. Index: `idx_class_handlers_universe (universe_id)`.

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

### builder_regions (legacy)

Still created for backward compatibility, but no code reads or writes it.
Path grants replaced it.

| Column | Type | Notes |
|--------|------|-------|
| account_id | TEXT NOT NULL | No foreign key |
| region_id | TEXT NOT NULL | No foreign key |

Primary key: `(account_id, region_id)`.

## Economy

### credits

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| player_id | TEXT NOT NULL REFERENCES accounts(id) | An account id, not a player object path |
| balance | INTEGER NOT NULL DEFAULT 0 | |

Unique: `(universe_id, player_id)`.

## Combat

### combat_state

HP and combat stats for entities that have been in combat.

| Column | Type | Notes |
|--------|------|-------|
| entity_id | TEXT PRIMARY KEY | Object id (NPC or player) |
| universe_id | TEXT NOT NULL | No foreign key |
| hp | INTEGER NOT NULL | |
| max_hp | INTEGER NOT NULL | |
| armor_class | INTEGER NOT NULL DEFAULT 10 | |
| attack_bonus | INTEGER NOT NULL DEFAULT 0 | |

### active_effects

Status effects (poison, stun, ...).

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| entity_id | TEXT NOT NULL REFERENCES combat_state(entity_id) ON DELETE CASCADE | |
| effect_type | TEXT NOT NULL | |
| remaining_ticks | INTEGER NOT NULL | |
| magnitude | INTEGER NOT NULL DEFAULT 0 | |
| damage_type | TEXT | For damage-over-time effects |
| source_id | TEXT | Who applied it |

Index: `idx_active_effects_entity (entity_id)`.

## Timers

### timers

Pending one-shot `game.call_out` timers. Heartbeats (`game.set_heart_beat`)
are kept in memory only and have no table.

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL REFERENCES universes(id) | |
| object_id | TEXT NOT NULL | Object whose handler is called. No foreign key. |
| method | TEXT NOT NULL | Handler name |
| fire_at | INTEGER NOT NULL | Unix time |
| args | TEXT | JSON |
| created_at | TEXT NOT NULL DEFAULT (datetime('now')) | |

Index: `idx_timers_fire_at (fire_at)`.

## Raft

Used by the OpenRaft log store. Game writes go through Raft and are applied to
the tables above.

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

## Startup Validation

`mudd` opens an existing database only. It does not create the database or
run migrations. It checks that these tables exist:

`accounts`, `universes`, `objects`, `code_store`, `credits`, `classes`,
`class_properties`, `class_handlers`, `timers`, `raft_log`, `raft_vote`, `raft_meta`.

It also checks that an `admin` or `owner` account exists. If either check fails, run
`mudd_init` against the database.

## Index Summary

| Index | Table (columns) |
|-------|-----------------|
| idx_objects_universe | objects(universe_id) |
| idx_objects_parent | objects(parent_id) |
| idx_objects_owner | objects(owner_id) |
| idx_path_grants_grantee | path_grants(universe_id, grantee_id) |
| idx_active_effects_entity | active_effects(entity_id) |
| idx_timers_fire_at | timers(fire_at) |
| idx_raft_log_term | raft_log(term) |
| idx_class_props_universe | class_properties(universe_id) |
| idx_class_handlers_universe | class_handlers(universe_id) |

SQLite also creates automatic indexes for the PRIMARY KEY and UNIQUE
constraints listed above.
