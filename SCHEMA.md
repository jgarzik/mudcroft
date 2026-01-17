# HemiMUD Database Schema

SQLite database schema for the HemiMUD game server.

## Core Tables

### accounts
User accounts and authentication.

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| username | TEXT UNIQUE NOT NULL | Login name |
| password_hash | TEXT | Argon2 password hash |
| salt | TEXT | Password salt |
| token | TEXT | Current session token |
| access_level | TEXT NOT NULL DEFAULT 'player' | player/builder/wizard/admin |
| created_at | TEXT | Timestamp |

### universes
Game universes (isolated game worlds).

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| name | TEXT NOT NULL | Display name |
| owner_id | TEXT NOT NULL | FK to accounts |
| config | TEXT DEFAULT '{}' | JSON configuration |
| theme_id | TEXT DEFAULT 'sierra-retro' | Visual theme |
| created_at | TEXT | Timestamp |

### objects
Game objects (rooms, items, NPCs, players).

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL | FK to universes |
| class | TEXT NOT NULL | Class name |
| parent_id | TEXT | FK to objects (container) |
| properties | TEXT DEFAULT '{}' | JSON properties |
| code_hash | TEXT | FK to code_store |
| created_at | TEXT | Timestamp |
| updated_at | TEXT | Timestamp |

## Class System

### classes
Custom class definitions (base classes are hardcoded).

| Column | Type | Description |
|--------|------|-------------|
| name | TEXT PRIMARY KEY | Class name |
| universe_id | TEXT NOT NULL | FK to universes |
| parent | TEXT | FK to classes (inheritance) |
| code_hash | TEXT | FK to code_store |
| created_at | TEXT | Timestamp |

### class_properties
Default properties for custom classes.

| Column | Type | Description |
|--------|------|-------------|
| class_name | TEXT NOT NULL | FK to classes (CASCADE) |
| universe_id | TEXT NOT NULL | FK to universes |
| key | TEXT NOT NULL | Property name |
| value | TEXT NOT NULL | JSON-encoded value |
| PRIMARY KEY | (class_name, key) | |

### class_handlers
Handler methods defined by custom classes.

| Column | Type | Description |
|--------|------|-------------|
| class_name | TEXT NOT NULL | FK to classes (CASCADE) |
| universe_id | TEXT NOT NULL | FK to universes |
| handler | TEXT NOT NULL | Handler name (on_init, etc.) |
| PRIMARY KEY | (class_name, handler) | |

## Code Storage

### code_store
Content-addressed Lua source code.

| Column | Type | Description |
|--------|------|-------------|
| hash | TEXT PRIMARY KEY | SHA-256 hash |
| source | TEXT NOT NULL | Lua source code |
| created_at | TEXT | Timestamp |

### image_store
Content-addressed image storage.

| Column | Type | Description |
|--------|------|-------------|
| hash | TEXT PRIMARY KEY | SHA-256 hash |
| data | BLOB NOT NULL | Image binary data |
| mime_type | TEXT NOT NULL | MIME type |
| size_bytes | INTEGER NOT NULL | File size |
| source | TEXT | Generation source |
| created_at | TEXT | Timestamp |
| reference_count | INTEGER DEFAULT 0 | Usage count |

## Economy

### credits
Player currency balances per universe.

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL | FK to universes |
| player_id | TEXT NOT NULL | FK to accounts |
| balance | INTEGER DEFAULT 0 | Credit balance |
| UNIQUE | (universe_id, player_id) | |

## Combat System

### combat_state
Entity combat statistics.

| Column | Type | Description |
|--------|------|-------------|
| entity_id | TEXT PRIMARY KEY | Object or player ID |
| universe_id | TEXT NOT NULL | FK to universes |
| hp | INTEGER NOT NULL | Current hit points |
| max_hp | INTEGER NOT NULL | Maximum hit points |
| armor_class | INTEGER DEFAULT 10 | AC |
| attack_bonus | INTEGER DEFAULT 0 | Attack modifier |

### active_effects
Status effects on entities (poison, stun, etc.).

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| entity_id | TEXT NOT NULL | FK to combat_state |
| effect_type | TEXT NOT NULL | Effect name |
| remaining_ticks | INTEGER NOT NULL | Duration |
| magnitude | INTEGER DEFAULT 0 | Effect power |
| damage_type | TEXT | For DoT effects |
| source_id | TEXT | Who applied it |

## Timers

### timers
One-shot timers (call_out).

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PRIMARY KEY | UUID |
| universe_id | TEXT NOT NULL | FK to universes |
| object_id | TEXT NOT NULL | Target object |
| method | TEXT NOT NULL | Handler to call |
| fire_at | INTEGER NOT NULL | Unix timestamp |
| args | TEXT | JSON arguments |
| created_at | TEXT | Timestamp |

## Permissions

### builder_regions
Builder access to specific regions.

| Column | Type | Description |
|--------|------|-------------|
| account_id | TEXT NOT NULL | FK to accounts |
| region_id | TEXT NOT NULL | Region object ID |
| PRIMARY KEY | (account_id, region_id) | |

## Settings

### universe_settings
Key-value settings per universe.

| Column | Type | Description |
|--------|------|-------------|
| universe_id | TEXT NOT NULL | FK to universes |
| key | TEXT NOT NULL | Setting name |
| value | TEXT NOT NULL | Setting value |
| PRIMARY KEY | (universe_id, key) | |

## Raft Consensus

### raft_log
Replicated log entries.

| Column | Type | Description |
|--------|------|-------------|
| log_index | INTEGER PRIMARY KEY | Log position |
| term | INTEGER NOT NULL | Raft term |
| entry_type | TEXT NOT NULL | Entry type |
| payload | TEXT | JSON payload |
| created_at | INTEGER | Unix timestamp |

### raft_vote
Current vote state (singleton).

| Column | Type | Description |
|--------|------|-------------|
| id | INTEGER PRIMARY KEY CHECK (id = 1) | Always 1 |
| term | INTEGER NOT NULL | Current term |
| node_id | INTEGER | Voted for |
| committed | INTEGER DEFAULT 0 | Commit index |

### raft_meta
Raft metadata key-value store.

| Column | Type | Description |
|--------|------|-------------|
| key | TEXT PRIMARY KEY | Metadata key |
| value | TEXT NOT NULL | Metadata value |

## Indexes

- `idx_objects_universe` on objects(universe_id)
- `idx_objects_parent` on objects(parent_id)
- `idx_timers_fire_at` on timers(fire_at)
- `idx_active_effects_entity` on active_effects(entity_id)
- `idx_raft_log_term` on raft_log(term)
- `idx_class_props_universe` on class_properties(universe_id)
- `idx_class_handlers_universe` on class_handlers(universe_id)
