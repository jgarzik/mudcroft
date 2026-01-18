# HemiMUD Design Specification

**Version:** 1.6.0 | **Updated:** 2025-01-16 | **Status:** Active Development

## 1. Executive Summary

Modern MUD game integrating:
- **Blockchain payments** via Hemi Payment Hub (see separate spec)
- **AI visuals** via Venice AI (OpenAI-compatible, DIEM tokens)
- **Programmable logic** via sandboxed Lua
- **Multi-universe** architecture on shared infrastructure

### Core Principles

1. **Code is Content**: Game mechanics are Lua scripts players can modify
2. **Pull Payments**: Users deposit collateral, vendors pull funds (inverts push model)
3. **Economic Sustainability**: All resource-consuming actions cost credits
4. **Wallet = Identity**: Ethereum wallet is permanent cross-universe identity
5. **Universe Economic Isolation**: Credits are universe-scoped, crypto→credits is one-way

### Identity Model

- Global layer: Account (wallet), friends, platform subscription
- Per-universe: Separate player object, credits, stats, inventory
- Cross-universe: Whisper/mail via shared messaging

## 2. Technology Stack

| Component | Technology | Purpose |
|-----------|------------|---------|
| Game Server | Rust (tokio, axum, openraft) | WebSocket, Lua execution, Raft consensus |
| Client | TypeScript, React, Vite, Viem | Browser UI, wallet |
| Database | SQLite (Raft-replicated) | Per-universe storage |
| Payments | Hemi Payment Hub (see separate spec) | Pull-payment infrastructure |
| LLM/Image | Venice AI | Text/image generation |
| Auth | JWT + wallet signatures | Sessions |

### Monorepo Structure

```
hemimud/
├── README.md
├── Makefile                    # Top-level build/test/deploy commands
├── docker-compose.yml          # Local dev cluster
├── .github/
│   └── workflows/
│       ├── ci.yml              # cargo test, npm test, forge test
│       └── deploy.yml
│
├── docs/
│   ├── hemimud-design-spec.md
│   └── hemi-payment-hub-spec.md
│
├── server/                     # Rust game server
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs
│   │   ├── config.rs
│   │   ├── error.rs
│   │   ├── db/
│   │   │   ├── mod.rs
│   │   │   ├── shared.rs       # Accounts, universe registry
│   │   │   ├── universe.rs     # Per-universe schema
│   │   │   └── migrations/
│   │   ├── raft/
│   │   │   ├── mod.rs
│   │   │   ├── state_machine.rs
│   │   │   ├── log_entry.rs
│   │   │   └── snapshot.rs
│   │   ├── lua/
│   │   │   ├── mod.rs
│   │   │   ├── sandbox.rs
│   │   │   ├── game_api.rs     # game.* functions exposed to Lua
│   │   │   ├── metering.rs
│   │   │   └── stdlib/         # Standard Lua libraries (simul_efuns)
│   │   │       ├── simul_efuns.lua  # this_player(), environment(), say(), etc.
│   │   │       ├── living.lua       # Living mixin (heart_beat, combat)
│   │   │       ├── combat.lua       # Combat system
│   │   │       ├── commands.lua     # Standard verbs (take, drop, look, go)
│   │   │       ├── room.lua         # Room behaviors, exits
│   │   │       ├── weapon.lua       # Weapon base class
│   │   │       ├── armor.lua        # Armor base class
│   │   │       ├── container.lua    # Container behaviors
│   │   │       └── tests/
│   │   │           └── game_logic_tests.lua
│   │   ├── net/
│   │   │   ├── mod.rs
│   │   │   ├── http.rs         # REST endpoints
│   │   │   ├── websocket.rs
│   │   │   └── protocol.rs     # Message types
│   │   ├── auth/
│   │   │   ├── mod.rs
│   │   │   ├── wallet.rs       # Signature verification
│   │   │   └── jwt.rs
│   │   ├── payments/
│   │   │   ├── mod.rs
│   │   │   └── hub_client.rs   # Payment Hub integration
│   │   ├── venice/
│   │   │   ├── mod.rs
│   │   │   ├── client.rs       # OpenAI-compatible API
│   │   │   └── room_image.rs   # Two-step generation
│   │   └── universe/
│   │       ├── mod.rs
│   │       ├── objects.rs
│   │       ├── rooms.rs
│   │       └── tasks.rs        # Scheduled tasks, timers
│   └── tests/
│       ├── harness/
│       │   ├── mod.rs
│       │   ├── server.rs
│       │   └── cluster.rs
│       ├── mocks/
│       │   ├── mod.rs
│       │   ├── venice.rs
│       │   └── payment_hub.rs
│       ├── fixtures/
│       │   ├── mod.rs
│       │   ├── world.rs
│       │   ├── combat.rs
│       │   └── wallets.rs
│       ├── common.rs
│       ├── test_sandbox.rs
│       ├── test_objects.rs
│       ├── test_movement.rs
│       ├── test_combat.rs
│       ├── test_websocket.rs
│       ├── test_raft.rs
│       └── test_auth.rs
│
├── client/                     # React web client
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── index.html
│   ├── src/
│   │   ├── main.tsx
│   │   ├── App.tsx
│   │   ├── components/
│   │   │   ├── RoomCanvas.tsx
│   │   │   ├── CommandInput.tsx
│   │   │   ├── ChatLog.tsx
│   │   │   ├── StatusBar.tsx
│   │   │   └── WalletConnect.tsx
│   │   ├── hooks/
│   │   │   ├── useWebSocket.ts
│   │   │   ├── useGame.ts
│   │   │   └── usePaymentHub.ts
│   │   ├── stores/
│   │   │   ├── gameStore.ts    # Zustand
│   │   │   └── authStore.ts
│   │   ├── lib/
│   │   │   ├── protocol.ts     # Message types (shared with server)
│   │   │   ├── commands.ts
│   │   │   └── wallet.ts
│   │   └── styles/
│   └── tests/
│       └── e2e/
│           ├── playwright.config.ts
│           ├── auth.spec.ts
│           ├── gameplay.spec.ts
│           └── payments.spec.ts
│
├── contracts/                  # Solidity (Payment Hub)
│   ├── foundry.toml
│   ├── src/
│   │   ├── PaymentHub.sol
│   │   ├── PaymentHubLogic.sol
│   │   └── PriceOracle.sol
│   ├── script/
│   │   └── Deploy.s.sol
│   └── test/
│       ├── PaymentHub.t.sol
│       └── PriceOracle.t.sol
│
└── scripts/
    ├── dev-cluster.sh          # Spin up 3-node local cluster
    ├── migrate.sh              # DB migrations
    ├── seed-world.sh           # Create test universe with rooms
    └── deploy-contracts.sh     # Deploy to testnet/mainnet
```

### Build Commands

```makefile
# Makefile
.PHONY: all test dev deploy

all: build

build:
	cd server && cargo build --release
	cd client && npm run build
	cd contracts && forge build

test:
	cd server && cargo test
	cd client && npm test
	cd contracts && forge test

dev:
	docker-compose up -d
	cd server && cargo run &
	cd client && npm run dev

lint:
	cd server && cargo clippy
	cd client && npm run lint
	cd contracts && forge fmt --check
```

## 3. Payments

HemiMUD uses **Hemi Payment Hub** for blockchain payments. See separate spec: `hemi-payment-hub-spec.md`

**Integration points:**
- Platform vendor wallet: Receives universe creation fees, hosting fees
- Universe vendor wallet: Each universe owner registers as vendor, receives credit purchases
- Credit purchase: Client calls `hub.pull(user, token, usdAmount)` via universe's vendor wallet

### Credit System (HemiMUD-Specific)

Credits are universe-scoped, in-game currency. One-way conversion: crypto → credits (no withdrawal).

**Why one-way:**
- Avoids money transmitter regulations
- Simplifies accounting
- Prevents arbitrage between universes
- Credits are consumable game resources, not stored value

**Purchase flow:**
1. User clicks "Buy 1000 credits for $10"
2. Client calls Payment Hub `pull(user, USDC, 10_000000)`
3. On success: server credits user's universe account via Raft log entry
4. On failure: UI shows "top up collateral" message

## 4. Game Server Architecture

### Cluster Model

- 3+ nodes running OpenRaft
- One leader (read-write, executes Lua), N followers (read-only, apply SQL)
- No shared filesystem

### Leader-Only Execution

**Critical**: Only leader executes Lua. Followers apply pre-computed SQL mutations.

Flow:
1. Trigger (player command, bot timer, scheduled task)
2. Leader executes Lua immediately
3. Leader collects SQL mutations
4. Leader proposes `Mutations` to Raft
5. Followers apply SQL directly (no Lua)
6. Leader broadcasts events to room occupants

### Raft Log Entry Types

```rust
pub enum GameLogEntry {
    CreateUniverse { universe_id, name, owner_account_id, vendor_wallet, initial_config },
    DeleteUniverse { universe_id },
    Mutations { universe_id, sql_statements: Vec<String> },
    DepositCredits { universe_id, account_id, wallet_address, amount, tx_hash, block_number },
    StoreCode { universe_id, hash, code, author_account_id },
}
```

### Event-Driven Model (No Tick Loop)

Three trigger types on leader:

**(A) Player Input**: WebSocket → execute Lua → Raft propose → broadcast to room

**(B) Scheduled Tasks**: `tokio::spawn` for bot AI loops, timers, respawns

**(C) Room-Scoped Broadcast**: `HashMap<RoomId, Vec<ConnectionHandle>>` — only room occupants receive events

### Node Configuration

```toml
node_id = 1
peers = ["10.0.1.1:9000", "10.0.1.2:9000", "10.0.1.3:9000"]
owner_wallet = "0x..."  # Server owner receives platform fees
hemi_rpc_url = "https://rpc.hemi.network"
venice_api_key = "venice-..."
venice_api_url = "https://api.venice.ai/api/v1"
```

### Cold Start Recovery

1. New node requests snapshot from leader
2. Leader creates SQLite backup, compresses, streams
3. New node applies snapshot
4. Leader sends log entries since snapshot
5. New node catches up, joins as follower

### Failover

~500ms total disruption. Commands may timeout (client retries). No data loss.

## 5. Database Design

### Runtime Data Directory

```
/var/lib/hemimud/
├── raft/{log/, snapshots/, state.json}
├── shared.db          # Accounts, universe registry
└── universes/*.db     # Per-universe data
```

### Shared Database Schema

```sql
CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    wallet_address TEXT UNIQUE NOT NULL,
    username TEXT UNIQUE,
    created_at INTEGER NOT NULL,
    last_login INTEGER,
    is_banned BOOLEAN DEFAULT FALSE,
    platform_tier TEXT DEFAULT 'free'
);

CREATE TABLE universes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT UNIQUE NOT NULL,
    owner_account_id TEXT REFERENCES accounts(id),
    vendor_wallet TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    is_official BOOLEAN DEFAULT FALSE,
    status TEXT DEFAULT 'active'
);

CREATE TABLE universe_memberships (
    account_id TEXT REFERENCES accounts(id),
    universe_id TEXT REFERENCES universes(id),
    access_level TEXT NOT NULL DEFAULT 'player',
    joined_at INTEGER NOT NULL,
    player_object_id TEXT,
    PRIMARY KEY (account_id, universe_id)
);
```

### Universe Database Schema

```sql
CREATE TABLE universe_config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE credits (
    account_id TEXT PRIMARY KEY,
    balance INTEGER NOT NULL DEFAULT 0,
    lifetime_deposited INTEGER DEFAULT 0,
    lifetime_spent INTEGER DEFAULT 0
);

CREATE TABLE credit_transactions (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    amount INTEGER NOT NULL,
    type TEXT NOT NULL,  -- 'deposit', 'spend', 'refund', 'grant'
    spend_category TEXT,  -- 'object_creation', 'llm_fast', 'llm_image', etc.
    created_at INTEGER NOT NULL
);

CREATE TABLE objects (
    id TEXT PRIMARY KEY,
    class TEXT NOT NULL,
    parent_id TEXT REFERENCES objects(id),
    owner_id TEXT,
    owner_type TEXT,
    name TEXT NOT NULL,
    description TEXT,
    metadata TEXT,  -- JSON
    code_hash TEXT REFERENCES lua_code(hash),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE lua_code (
    hash TEXT PRIMARY KEY,
    code TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    reference_count INTEGER DEFAULT 0
);

CREATE TABLE rooms (
    object_id TEXT PRIMARY KEY REFERENCES objects(id),
    base_image_url TEXT,
    image_hash TEXT,
    exits TEXT  -- JSON: {"north": "room_id", ...}
);

CREATE TABLE permissions (
    id TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT,
    permission TEXT NOT NULL,
    UNIQUE(subject_type, subject_id, resource_type, resource_id, permission)
);

CREATE TABLE event_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL,
    actor_id TEXT,
    target_id TEXT,
    room_id TEXT,
    data TEXT,
    created_at INTEGER NOT NULL
);
```

## 6. Lua Runtime & Sandboxing

> Lua executes **only on leader node**.

### Sandbox Setup

Remove: `os`, `io`, `loadfile`, `dofile`, `load`. Install metering hook, safe stdlib, game API.

### Resource Limits

| Resource | Limit |
|----------|-------|
| Instructions/exec | 1,000,000 |
| Memory/sandbox | 64 MB |
| DB queries/exec | 100 |
| Execution timeout | 500ms |
| Venice calls/exec | 5 |
| Venice calls/min/bot | 60 |

### Cost Model

```lua
costs = {
    instruction_block = 0.0001,
    db_read = 0.001,
    db_write = 0.01,
    llm_tier_fast = 0.02,
    llm_tier_smart = 0.20,
    llm_tier_vision = 0.10,
    llm_image_gen = 0.05,
    room_image_gen = 0.07,
    object_create_base = 0.10,
}
```

### Content-Addressed Code Storage

All Lua code stored by SHA-256 hash. Enables deduplication, immutable history, safe updates, GC of unreferenced code.

## 7. Object & Class System (LPC-Aligned)

### Design Philosophy (from LP-MUD/LPC)

The key insight from LPC: **objects teach players their verbs**. When you enter a room, every object gets a chance to add contextual commands to you. This makes the world naturally interactive.

- `init()` cascade: When anything moves, affected objects exchange capabilities
- Living objects: Players and NPCs share a common base with heart_beat()
- Containment is fundamental: `environment()` and `all_inventory()` are core queries

### Object Hierarchy

```
thing (root)
├── name, description, metadata, code_hash
├── handlers: on_create, on_destroy, on_init, on_move
│
├── living (mixin - adds heart_beat, combat capability)
│   ├── health, max_health, stats
│   ├── in_combat, attackers, attacking
│   ├── handlers: heart_beat, on_damage, on_death
│   │
│   ├── player
│   │   ├── access_level, assigned_regions, wallet_address
│   │   ├── actions = {}  -- contextual verbs added by init()
│   │   └── handlers: on_disconnect
│   │
│   └── npc (AI-controlled living)
│       ├── ai_script, wander_area, aggro_range
│       └── handlers: on_attacked, on_idle
│
├── room
│   ├── exits = {north = room_id, ...}
│   ├── lighting, image_url
│   ├── handlers: on_enter, on_leave, on_reset
│   └── reset_interval (for respawns)
│
├── item
│   ├── weight, value, fixed
│   ├── handlers: on_init (adds contextual verbs to nearby living)
│   │
│   ├── weapon
│   │   ├── damage_dice, damage_bonus, damage_type
│   │   ├── elemental_damage_dice, elemental_damage_type
│   │   └── handlers: on_hit, on_wield, on_unwield
│   │
│   ├── armor
│   │   ├── armor_value, slot (head/chest/legs/feet/hands)
│   │   └── handlers: on_wear, on_remove
│   │
│   └── container
│       ├── capacity, locked, key_id
│       └── handlers: on_open, on_close, on_lock
│
└── region
    ├── environment_type, danger_level, ambient_sounds
    └── handlers: on_enter, on_leave (region-wide events)
```

### The Living Mixin

All players and NPCs share the `living` trait:

```lua
Living = {
    -- Core stats
    health = 10,
    max_health = 10,
    
    -- Combat state
    in_combat = false,
    attacking = nil,      -- Current target id
    attackers = {},       -- Set of ids attacking us
    
    -- Called every heart_beat interval (default 2000ms)
    heart_beat = function(self)
        -- Override for: AI logic, regen, combat rounds, status tick
    end,
    
    is_living = function(self) return true end,
}

-- Enable/disable heartbeat
game.set_heart_beat(obj_id, interval_ms)  -- 0 to disable
```

### Object Properties

```lua
-- Standard metadata properties
metadata = {
    -- Movement/interaction
    fixed = false,           -- If true, cannot be picked up or moved by players
    weight = 1,              -- Affects carry capacity
    
    -- Combat stats (living only)
    health = 10,
    max_health = 10,
    armor_class = 10,
    attack_bonus = 0,
    
    -- Damage modifiers
    immunities = {},         -- { fire = true, poison = true }
    resistances = {},        -- Half damage: { fire = true }
    vulnerabilities = {},    -- Double damage: { cold = true }
}
```

### The init() Pattern (Critical)

When any object moves (enters a new environment), an `init()` cascade occurs:

1. Destination container's `on_enter(newcomer)` fires
2. All objects at destination get `on_init(newcomer)` called
3. The newcomer gets `on_init(destination)` and `on_init(each_sibling)` called

**This is how verbs become contextual:**

```lua
-- sword.lua
function Sword:on_init(other)
    if not other:is_living() then return end
    
    local my_env = game.environment(self.id)
    local their_env = game.environment(other.id)
    
    if my_env.id == other.id then
        -- Sword is in this living's inventory
        game.add_action(other.id, "wield " .. self.name, function()
            return self:do_wield(other)
        end)
        game.add_action(other.id, "drop " .. self.name, function()
            return Commands.drop(other, self)
        end)
    elseif my_env.id == their_env.id then
        -- Sword is in same room as this living
        game.add_action(other.id, "get " .. self.name, function()
            return Commands.take(other, self)
        end)
    end
end

function Sword:on_move(from_env, to_env)
    -- Clean up actions from all living things at old location
    for _, obj in ipairs(game.all_inventory(from_env.id)) do
        if obj:is_living() then
            game.remove_action(obj.id, "wield " .. self.name)
            game.remove_action(obj.id, "get " .. self.name)
            game.remove_action(obj.id, "drop " .. self.name)
        end
    end
end
```

**Room exits work the same way:**

```lua
-- room.lua
function Room:on_init(newcomer)
    if not newcomer:is_living() then return end
    
    for direction, dest_id in pairs(self.metadata.exits) do
        game.add_action(newcomer.id, direction, function()
            return Commands.go(newcomer, direction)
        end)
    end
end
```

### Object Lifecycle Events

| Handler | When Called | Receives |
|---------|-------------|----------|
| `on_create(self)` | Object instantiated | - |
| `on_destroy(self)` | Object being deleted | - |
| `on_init(self, other)` | Something entered my env (or I entered theirs) | The other object |
| `on_move(self, from, to)` | I'm being moved | Old and new environment |
| `on_enter(self, who)` | Someone entered me (rooms/containers) | The entrant |
| `on_leave(self, who)` | Someone left me (rooms/containers) | The departer |
| `on_use(self, actor, verb)` | Actor interacted with me | Actor and verb string |
| `on_hit(self, attacker, defender, result)` | I (weapon) hit something | Combat participants |
| `on_damage(self, amount, type, source)` | I took damage | Damage info |
| `on_death(self, killer)` | I reached 0 HP | Killer (or nil) |
| `heart_beat(self)` | Periodic tick (living only) | - |
| `on_reset(self)` | Room reset timer fired | - |

### Class Inheritance (LPC Core Pattern)

Inheritance is fundamental. Every class inherits from a parent, forming chains:

```
thing (root)
  └── item
        └── weapon
              └── sword
                    └── fire_sword
                          └── legendary_fire_sword
```

**Properties cascade down:** Child inherits all parent properties, can override defaults.

**Handlers cascade down:** Child inherits all parent handlers, can override or extend via `parent()`.

### Built-in Class Hierarchy

```lua
-- Base classes provided by stdlib (cannot be redefined)

-- thing: Root of all objects
game.define_class("thing", {
    parent = nil,
    properties = {
        name = { type = "string", default = "thing" },
        description = { type = "string", default = "" },
    },
    handlers = {
        on_create = function(self) end,
        on_destroy = function(self) end,
        on_init = function(self, other) end,
        on_move = function(self, from, to) end,
    }
})

-- item: Physical objects that can be in rooms/inventory
game.define_class("item", {
    parent = "thing",
    properties = {
        weight = { type = "number", default = 1 },
        value = { type = "number", default = 0 },
        fixed = { type = "boolean", default = false },
    },
    handlers = {
        on_init = function(self, other)
            -- Items add "get/drop/look" verbs - see init() pattern
            parent(self, other)  -- Call thing's on_init first
            if other:is_living() then
                -- Add contextual verbs...
            end
        end,
    }
})

-- weapon: Items that can be wielded for combat
game.define_class("weapon", {
    parent = "item",
    properties = {
        damage_dice = { type = "string", default = "1d4" },
        damage_bonus = { type = "number", default = 0 },
        damage_type = { type = "string", default = "physical" },
        two_handed = { type = "boolean", default = false },
    },
    handlers = {
        on_init = function(self, other)
            parent(self, other)  -- Call item's on_init (which calls thing's)
            if other:is_living() and environment(self).id == other.id then
                game.add_action(other.id, "wield " .. self.name, function()
                    return self:do_wield(other)
                end)
            end
        end,
        on_hit = function(self, attacker, defender, result)
            -- Base weapon hit - just physical damage
            return result
        end,
        on_wield = function(self, wielder) end,
        on_unwield = function(self, wielder) end,
    }
})

-- sword: A type of weapon with sword-specific behavior
game.define_class("sword", {
    parent = "weapon",
    properties = {
        damage_dice = { type = "string", default = "1d8" },  -- Override: swords do more
        blade_type = { type = "string", default = "long" },  -- New property
    },
    handlers = {
        on_hit = function(self, attacker, defender, result)
            result = parent(self, attacker, defender, result)  -- Call weapon's on_hit
            -- Swords have chance to cause bleeding
            if result.hit and game.random(1, 10) == 1 then
                game.apply_status(defender.id, "bleeding", { duration = 5 })
            end
            return result
        end,
    }
})
```

### Defining Custom Classes (User/Builder Code)

```lua
-- fire_sword: Inherits sword → weapon → item → thing
game.define_class("fire_sword", {
    parent = "sword",
    properties = {
        damage_bonus = { type = "number", default = 1 },           -- +1 sword
        elemental_damage_dice = { type = "string", default = "1d6" },
        elemental_damage_type = { type = "string", default = "fire" },
        glow = { type = "boolean", default = true },               -- New property
    },
    handlers = {
        on_create = function(self)
            parent(self)  -- Call sword's on_create chain
            game.broadcast(environment(self).id, "A fiery glow emanates from " .. self.name)
        end,
        on_hit = function(self, attacker, defender, result)
            -- IMPORTANT: Call parent chain first (sword → weapon → item → thing)
            result = parent(self, attacker, defender, result)
            
            if result.hit then
                -- Add elemental damage (handled by combat system with immunities)
                local elem_dice = self.metadata.elemental_damage_dice
                local elem_type = self.metadata.elemental_damage_type
                local elem_damage = Combat.roll(elem_dice)
                local elem_result = Combat.deal_damage(defender.id, elem_damage, elem_type)
                
                result.elemental_damage = elem_result.applied
                result.elemental_type = elem_type
                
                if elem_result.applied > 0 then
                    game.broadcast(environment(attacker).id, 
                        "Flames leap from " .. self.name .. " burning " .. defender.name .. "!")
                end
            end
            return result
        end,
    }
})

-- legendary_fire_sword: Even more specific
game.define_class("legendary_fire_sword", {
    parent = "fire_sword",
    properties = {
        damage_bonus = { type = "number", default = 3 },           -- +3 instead of +1
        elemental_damage_dice = { type = "string", default = "2d6" },  -- More fire
        sentient = { type = "boolean", default = true },
        sword_name = { type = "string", default = "Flamebrand" },
    },
    handlers = {
        on_wield = function(self, wielder)
            parent(self, wielder)
            game.send(wielder.id, self.metadata.sword_name .. " speaks to your mind: 'Together we shall burn our enemies!'")
        end,
    }
})
```

### The `parent()` Function

Calling `parent(self, ...)` invokes the parent class's handler with the same arguments:

```lua
on_hit = function(self, attacker, defender, result)
    -- This calls sword's on_hit, which calls weapon's, which calls item's, etc.
    result = parent(self, attacker, defender, result)
    
    -- Now add our custom behavior
    ...
    return result
end
```

**If you don't call `parent()`**, the parent's handler is skipped entirely. This is intentional - sometimes you want to completely replace behavior.

### Inheritance Resolution

When `game.create_object("/items/fire-sword-1", "fire_sword", room.id, props)` is called:

1. **Property resolution:** Merge defaults from chain (thing → item → weapon → sword → fire_sword)
2. **Handler resolution:** Each handler checks parent chain at runtime via `parent()`
3. **Instance creation:** Store merged properties + class reference in SQLite

```lua
-- Creating an instance with path-based ID
local flaming = game.create_object("/items/inferno-blade", "fire_sword", room.id, {
    name = "Inferno Blade",
    description = "A sword wreathed in eternal flames.",
    metadata = {
        damage_bonus = 2,  -- Override: this specific sword is +2
        -- Other properties inherit from fire_sword → sword → weapon → item → thing
    }
})

-- flaming now has:
--   name = "Inferno Blade" (overridden)
--   weight = 1 (from item)
--   damage_dice = "1d8" (from sword)
--   damage_bonus = 2 (overridden, was 1 from fire_sword)
--   elemental_damage_dice = "1d6" (from fire_sword)
--   glow = true (from fire_sword)
```

### Querying Inheritance

```lua
local info = game.get_class("fire_sword")
-- info.parent = "sword"
-- info.ancestors = {"sword", "weapon", "item", "thing"}
-- info.properties = { merged properties with sources }

game.is_a(obj.id, "weapon")  -- true for fire_sword instances
game.is_a(obj.id, "armor")   -- false

game.get_class_chain("fire_sword")  
-- Returns: {"fire_sword", "sword", "weapon", "item", "thing"}
```

## 8. Visual Rendering Pipeline

### Two-Step Image Generation

1. **Text LLM** (fast tier, ~$0.02): Server provides visual theory + room data + creator hints → LLM crafts optimal image prompt
2. **Image LLM** (fluently-xl, ~$0.05): Execute crafted prompt

### Visual Theory

Server-wide constant defining aesthetic: Sierra adventure game (King's Quest era), 320x200 VGA, 256-color, dithered shading, painterly pixels, 3/4 elevated view. No people in backgrounds.

### Layering

| Layer | Contents | Generation |
|-------|----------|------------|
| Background | Room structure | Venice 2-step |
| Objects | Items, furniture | Sprites or Venice |
| Entities | Players, bots | Client-side sprites |
| UI | Names, health bars | Client-side |

### Fallback

When Venice unavailable: text-only, procedural gradients, or sprite-only.

## 9. Client Architecture

### Stack

React 18+, TypeScript, Vite, Zustand (state), Viem + wagmi (blockchain), HTML5 Canvas (room rendering)

### WebSocket Protocol

```typescript
// Client → Server
type ClientMessage = 
  | { type: "auth", token: string }
  | { type: "join_universe", universeId: string }
  | { type: "command", text: string }
  | { type: "ping" }

// Server → Client
type ServerMessage =
  | { type: "auth_ok", playerId, universeId }
  | { type: "room_state", room: RoomState }
  | { type: "event", event: GameEvent }
  | { type: "error", code, message }
```

### Room State

```typescript
interface RoomState {
  roomId: string;
  name: string;
  description: string;
  players: PlayerSummary[];
  bots: BotSummary[];
  objects: ObjectSummary[];
  exits: Exit[];
  aggregated?: boolean;  // True if 100+ occupants
}
```

## 10. Pricing & Economics

### Credit Packages (Universe-Configurable)

```json
[
  { "usd": 5, "credits": 500 },
  { "usd": 20, "credits": 2200, "bonus_pct": 10 },
  { "usd": 100, "credits": 12000, "bonus_pct": 20 }
]
```

### Default Costs

| Action | Cost |
|--------|------|
| Venice fast tier | 2¢ |
| Venice smart tier | 20¢ |
| Venice vision tier | 10¢ |
| Venice image | 5¢ |
| Room image (2-step) | 7¢ |
| Object creation | 10¢ |
| Bot monthly fee | 50¢ |
| Class definition | $50 |

### Cost Flow

Players pay **credits** (in-game) to universe. Server operator pays **DIEM tokens** to Venice for API calls.

## 11. Security Model

### Permission Levels

| Level | Capabilities |
|-------|-------------|
| player | Move, interact, own inventory, run allowed commands |
| builder | Create/edit rooms and objects in `assigned_regions` only |
| wizard | Execute arbitrary code, create regions, moderate players |
| admin | Universe config, billing, user management |
| owner | Full control, can delete universe |

```lua
-- Builder with assigned regions
player.metadata = {
    access_level = "builder",
    assigned_regions = { 
        ["region_abc123"] = true,
        ["region_def456"] = true
    }
}

-- Permission check examples
game.check_permission(actor_id, "create_region", nil)      -- wizard+ only
game.check_permission(actor_id, "create_room", region_id)  -- builder in region
game.check_permission(actor_id, "move_fixed", object_id)   -- wizard+ only
game.check_permission(actor_id, "take", object_id)         -- player+ (if not fixed)
```

### Lua Security

- No filesystem/network access
- Instruction counting with abort
- Memory limits
- CPU time limits
- Allowlist of safe functions

### Rate Limits

Per-account: 60 commands/min, 10 LLM calls/min
Per-bot: 60 Venice calls/min

## 12. Standard Lua Library (LPC-Aligned)

### Object Operations

```lua
game.get_object(id) -> object
game.create_object(path, class, parent_id, props) -> object  -- Path-based ID (e.g., "/items/sword")
game.clone_object(obj_id, new_path, new_parent_id) -> object  -- Copy with new path
game.update_object(id, changes)
game.delete_object(id)
game.move_object(id, new_parent_id)  -- Triggers init() cascade
game.use_object(obj_id, actor_id, verb) -> result
```

### Environment Queries (LPC-style)

```lua
game.environment(obj_id) -> object          -- Where is this object?
game.all_inventory(obj_id) -> objects[]     -- What's inside this object?
game.deep_inventory(obj_id) -> objects[]    -- Recursive contents
game.present(name, env_id) -> object        -- Find by name in environment
game.present_living(name, env_id) -> object -- Find living by name

-- Aliases for database-style naming
game.get_parent = game.environment
game.get_children = game.all_inventory
```

### Contextual Actions (The init() System)

```lua
-- Add verb to a specific player (called from on_init)
game.add_action(player_id, verb_string, callback)
game.remove_action(player_id, verb_string)
game.get_actions(player_id) -> { verb = callback, ... }

-- Example: sword adds "get sword" when player enters room
function Sword:on_init(other)
    if other:is_living() and game.environment(self.id).id == game.environment(other.id).id then
        game.add_action(other.id, "get " .. self.name, function()
            return Commands.take(other, self)
        end)
    end
end
```

### Global Commands (Non-contextual)

```lua
-- These work regardless of location (system commands)
game.register_command(name, handler_fn)  -- "quit", "who", "help", etc.
game.unregister_command(name)
```

### Class Operations and Inheritance

```lua
-- Class definition
game.define_class(name, definition)    -- Define new class with parent
game.get_class(name) -> class_info     -- Get class definition
game.has_class(name) -> boolean
game.update_class(name, changes)       -- Affects future instances

-- Inheritance queries
game.is_a(obj_id, class_name) -> boolean   -- Is this object a kind of X?
game.get_class_chain(class_name) -> string[]  -- ["fire_sword", "sword", "weapon", "item", "thing"]
game.get_ancestors(class_name) -> string[]    -- ["sword", "weapon", "item", "thing"]

-- In handlers: call parent implementation
parent(self, ...)  -- Calls parent class's handler with same args
```

### Living Object Support

```lua
game.set_heart_beat(obj_id, interval_ms)  -- 0 to disable
game.is_living(obj_id) -> boolean
game.get_living_in(env_id) -> living[]    -- All living things in environment
```

### call_out (Named Delayed Callbacks)

```lua
-- LPC-style named callbacks (better than anonymous timers)
game.call_out(obj_id, method_name, delay_seconds, ...)  -- Call obj:method(...) later
game.remove_call_out(obj_id, method_name)
game.find_call_out(obj_id, method_name) -> remaining_seconds or nil

-- Example: bomb with defuse option
function Bomb:on_create()
    game.call_out(self.id, "explode", 30)
end

function Bomb:explode()
    game.broadcast(game.environment(self.id).id, "BOOM!")
    for _, obj in ipairs(game.get_living_in(game.environment(self.id).id)) do
        Combat.deal_damage(obj.id, Combat.roll("4d6"), "fire")
    end
    game.delete_object(self.id)
end

function Bomb:on_use(actor, verb)
    if verb == "defuse" then
        game.remove_call_out(self.id, "explode")
        return { success = true, message = "You defuse the bomb." }
    end
end
```

### Communication

```lua
game.send(target_id, message)               -- Private message to one entity
game.broadcast(room_id, message)            -- To all in room
game.broadcast_except(room_id, except_id, message)
game.broadcast_region(region_id, message)   -- To all in region (shout)
```

### Simul_efuns (Convenience Functions)

```lua
-- Loaded automatically, feel like built-ins
this_player()          -- Current actor (game.get_actor())
this_object()          -- Current object being executed
environment(obj)       -- game.environment(), defaults to this_object()
all_inventory(obj)     -- game.all_inventory(), defaults to this_object()

say(msg)               -- Broadcast to room: "Name says: msg"
tell(target, msg)      -- Private message
shout(msg)             -- Broadcast to region
emote(action)          -- Broadcast: "Name action"
```

### Code Storage

```lua
game.store_code(lua_code) -> hash
game.get_code(hash) -> lua_code
game.set_object_code(obj_id, code_hash)
```

### Status Effects

```lua
game.apply_status(entity_id, status_name, options)
game.remove_status(entity_id, status_name)
game.has_status(entity_id, status_name) -> boolean
game.get_statuses(entity_id) -> status_table
```

### LLM Integration

```lua
game.llm_chat({ tier = "fast"|"smart"|"vision", messages = {...} })
game.llm_image({ prompt, style, size })
game.generate_room_image(room_id, overrides)
```

### Time and Random

```lua
game.time() -> unix_ms
game.random(min, max)          -- Seeded per-execution
game.set_rng_seed(seed)        -- Testing only
game.set_time(unix_ms)         -- Testing only
game.advance_time(delta_ms)    -- Testing only
```

### Actor Context

```lua
game.set_actor(entity_id)
game.get_actor() -> entity_id
game.check_permission(actor_id, action, target_id) -> boolean
```

### Standard Commands Module

```lua
-- commands.lua - Standard player commands
-- These are called by contextual actions set up via init()

Commands = {}

function Commands.take(actor, target)
    -- Check if object is fixed
    if target.metadata.fixed then
        return { success = false, message = "The " .. target.name .. " is fixed in place." }
    end
    
    -- Check weight
    local carry = actor.metadata.carry_weight or 0
    local max = actor.metadata.max_carry or 100
    local weight = target.metadata.weight or 1
    
    if carry + weight > max then
        return { success = false, message = "You cannot carry that much." }
    end
    
    -- Move to inventory (triggers init() cascade - sword adds wield/drop verbs)
    local old_env = environment(target)
    game.move_object(target.id, actor.id)
    
    actor.metadata.carry_weight = carry + weight
    game.update_object(actor.id, { metadata = actor.metadata })
    
    game.broadcast_except(old_env.id, actor.id, actor.name .. " picks up " .. target.name)
    return { success = true, message = "You pick up the " .. target.name .. "." }
end

function Commands.drop(actor, target)
    if environment(target).id ~= actor.id then
        return { success = false, message = "You are not carrying that." }
    end
    
    local room = environment(actor)
    game.move_object(target.id, room.id)  -- Triggers init() - removes wield/drop, adds get
    
    local weight = target.metadata.weight or 1
    actor.metadata.carry_weight = (actor.metadata.carry_weight or 0) - weight
    game.update_object(actor.id, { metadata = actor.metadata })
    
    game.broadcast_except(room.id, actor.id, actor.name .. " drops " .. target.name)
    return { success = true, message = "You drop the " .. target.name .. "." }
end

function Commands.go(actor, direction)
    local room = environment(actor)
    local dest_id = room.metadata.exits and room.metadata.exits[direction]
    
    if not dest_id then
        return { success = false, message = "You cannot go " .. direction .. "." }
    end
    
    game.broadcast_except(room.id, actor.id, actor.name .. " leaves " .. direction .. ".")
    game.move_object(actor.id, dest_id)  -- Triggers init() cascade at new room
    game.broadcast_except(dest_id, actor.id, actor.name .. " arrives.")
    
    return Commands.look(actor, nil)
end

function Commands.look(actor, target)
    if target then
        return { success = true, message = target.description or "You see nothing special." }
    end
    
    local room = environment(actor)
    local contents = all_inventory(room)
    local living = {}
    local items = {}
    
    for _, obj in ipairs(contents) do
        if obj.id ~= actor.id then
            if game.is_living(obj.id) then
                table.insert(living, obj)
            else
                table.insert(items, obj)
            end
        end
    end
    
    return { 
        success = true, 
        room = room,
        living = living,
        items = items,
        exits = room.metadata.exits or {}
    }
end

-- Global commands (always available)
game.register_command("quit", function(actor)
    game.disconnect(actor.id)
end)

game.register_command("who", function(actor)
    local players = game.get_all_players()
    return { success = true, players = players }
end)
```

### Combat System (LPC-Aligned)

The combat system shares a single pipeline for PvM and PvP. PvP is a **policy layer** on top of the same mechanics.

#### Combat State (on Living objects)

```lua
-- Every living object tracks combat state
living.metadata = {
    -- Combat engagement
    in_combat = false,
    attacking = nil,          -- Current target id (who I'm attacking)
    attackers = {},           -- Set of ids attacking me: { [id] = true }
    combat_round = 0,         -- Current round number
    
    -- Combat stats
    health = 50,
    max_health = 50,
    armor_class = 10,
    attack_bonus = 0,
    
    -- Equipment slots
    wielded = nil,            -- Weapon id
    worn = {},                -- { head = id, chest = id, ... }
    
    -- AI state (NPCs only)
    aggro_range = 5,          -- Rooms away to detect enemies
    aggro_targets = {},       -- Types to auto-attack: { "player" = true }
    flee_threshold = 0.2,     -- Flee at 20% health
    assist_allies = true,     -- Help nearby friendlies
}
```

#### Combat Loop (Heartbeat-Driven)

Combat advances via the `heart_beat()` of living objects. This is the LPC pattern.

```lua
-- In living.lua - called every heart_beat interval (default 2000ms)
function Living:heart_beat()
    if not self.metadata.in_combat then
        -- Not in combat - NPC AI can look for targets
        if self.class == "npc" then
            self:ai_idle_tick()
        end
        return
    end
    
    -- In combat - execute combat round
    self.metadata.combat_round = (self.metadata.combat_round or 0) + 1
    
    -- Check if target still valid
    local target = self.metadata.attacking and game.get_object(self.metadata.attacking)
    if not target or target.metadata.health <= 0 then
        self:stop_combat()
        return
    end
    
    -- Check if target still in same room
    if environment(target).id ~= environment(self).id then
        self:stop_combat()
        game.send(self.id, "Your target has fled!")
        return
    end
    
    -- Execute attack
    local weapon = self.metadata.wielded and game.get_object(self.metadata.wielded)
    if not weapon then
        weapon = self:get_natural_weapon()  -- Fists, claws, etc.
    end
    
    Combat.attack_extended(self, target, weapon)
    
    -- NPC AI: flee check
    if self.class == "npc" then
        self:ai_combat_tick()
    end
end
```

#### Initiating Combat

```lua
function Combat.initiate(attacker, defender)
    -- Check if combat is allowed (PvP policy)
    if not Combat.can_attack(attacker, defender) then
        return { success = false, message = "You cannot attack that target." }
    end
    
    -- Set up combat state
    attacker.metadata.in_combat = true
    attacker.metadata.attacking = defender.id
    attacker.metadata.combat_round = 0
    
    defender.metadata.in_combat = true
    defender.metadata.attackers[attacker.id] = true
    
    -- Auto-retaliate if NPC
    if defender.class == "npc" and not defender.metadata.attacking then
        defender.metadata.attacking = attacker.id
    end
    
    game.update_object(attacker.id, { metadata = attacker.metadata })
    game.update_object(defender.id, { metadata = defender.metadata })
    
    -- Enable heartbeat if not already running
    game.set_heart_beat(attacker.id, 2000)
    game.set_heart_beat(defender.id, 2000)
    
    game.broadcast(environment(attacker).id, 
        attacker.name .. " attacks " .. defender.name .. "!")
    
    return { success = true }
end

function Combat.stop(entity)
    entity.metadata.in_combat = false
    entity.metadata.attacking = nil
    entity.metadata.combat_round = 0
    
    -- Remove self from all attackers' lists
    for attacker_id, _ in pairs(entity.metadata.attackers or {}) do
        local attacker = game.get_object(attacker_id)
        if attacker and attacker.metadata.attacking == entity.id then
            attacker.metadata.attacking = nil
            -- Attacker might pick new target or exit combat
            Combat.find_new_target(attacker)
        end
    end
    entity.metadata.attackers = {}
    
    game.update_object(entity.id, { metadata = entity.metadata })
    
    -- Disable heartbeat for players not in combat
    if entity.class == "player" and not entity.metadata.in_combat then
        game.set_heart_beat(entity.id, 0)
    end
end

function Combat.find_new_target(entity)
    -- Look for other valid targets in room
    local room = environment(entity)
    for _, obj in ipairs(game.get_living_in(room.id)) do
        if obj.metadata.attackers and obj.metadata.attackers[entity.id] then
            -- Someone is still attacking us, fight back
            entity.metadata.attacking = obj.id
            game.update_object(entity.id, { metadata = entity.metadata })
            return
        end
    end
    -- No targets, exit combat
    Combat.stop(entity)
end
```

#### PvP Policy Layer

```lua
-- PvP is controlled by policy, not different code paths
Combat.PVP_MODES = {
    DISABLED = "disabled",     -- No PvP anywhere
    ARENA_ONLY = "arena_only", -- Only in designated arenas
    FLAGGED = "flagged",       -- Only between flagged players
    OPEN = "open",             -- Full PvP everywhere
}

function Combat.can_attack(attacker, defender)
    -- PvM is always allowed
    if defender.class == "npc" then
        return true
    end
    
    -- PvP checks
    if attacker.class == "player" and defender.class == "player" then
        local universe = game.get_universe()
        local pvp_mode = universe.metadata.pvp_mode or Combat.PVP_MODES.DISABLED
        
        if pvp_mode == Combat.PVP_MODES.DISABLED then
            return false
        end
        
        if pvp_mode == Combat.PVP_MODES.ARENA_ONLY then
            local room = environment(attacker)
            return room.metadata.is_arena == true
        end
        
        if pvp_mode == Combat.PVP_MODES.FLAGGED then
            return attacker.metadata.pvp_flagged and defender.metadata.pvp_flagged
        end
        
        if pvp_mode == Combat.PVP_MODES.OPEN then
            return true
        end
    end
    
    return false
end

-- Player commands for PvP flagging
function Commands.pvp_on(actor)
    actor.metadata.pvp_flagged = true
    game.update_object(actor.id, { metadata = actor.metadata })
    game.broadcast(environment(actor).id, actor.name .. " is now flagged for PvP!")
    return { success = true, message = "You are now flagged for PvP combat." }
end

function Commands.pvp_off(actor)
    if actor.metadata.in_combat then
        return { success = false, message = "You cannot unflag while in combat." }
    end
    actor.metadata.pvp_flagged = false
    game.update_object(actor.id, { metadata = actor.metadata })
    return { success = true, message = "You are no longer flagged for PvP." }
end
```

#### NPC AI (Heartbeat-Driven)

```lua
function NPC:ai_idle_tick()
    -- Called during heartbeat when not in combat
    
    -- Check for aggro targets in room
    if self.metadata.aggro_targets then
        local room = environment(self)
        for _, obj in ipairs(game.get_living_in(room.id)) do
            if self.metadata.aggro_targets[obj.class] then
                Combat.initiate(self, obj)
                game.broadcast(room.id, self.name .. " growls and attacks " .. obj.name .. "!")
                return
            end
        end
    end
    
    -- Wander behavior
    if self.metadata.wander and game.random(1, 10) == 1 then
        local room = environment(self)
        local exits = room.metadata.exits or {}
        local directions = {}
        for dir, _ in pairs(exits) do
            table.insert(directions, dir)
        end
        if #directions > 0 then
            local dir = directions[game.random(1, #directions)]
            Commands.go(self, dir)
        end
    end
end

function NPC:ai_combat_tick()
    -- Called during heartbeat while in combat
    
    -- Flee check
    local health_pct = self.metadata.health / self.metadata.max_health
    if health_pct <= (self.metadata.flee_threshold or 0) then
        local room = environment(self)
        local exits = room.metadata.exits or {}
        for dir, dest_id in pairs(exits) do
            Combat.stop(self)
            Commands.go(self, dir)
            game.broadcast(room.id, self.name .. " flees " .. dir .. "!")
            return
        end
    end
    
    -- Assist check - help nearby allies
    if self.metadata.assist_allies then
        local room = environment(self)
        for _, ally in ipairs(game.get_living_in(room.id)) do
            if ally.class == "npc" and ally.id ~= self.id and ally.metadata.in_combat then
                -- Join ally's fight
                local target = game.get_object(ally.metadata.attacking)
                if target and not self.metadata.attacking then
                    self.metadata.attacking = target.id
                    target.metadata.attackers[self.id] = true
                    game.update_object(self.id, { metadata = self.metadata })
                    game.update_object(target.id, { metadata = target.metadata })
                    game.broadcast(room.id, self.name .. " joins the fight!")
                end
            end
        end
    end
end
```

#### Death Handling

```lua
function Combat.handle_death(victim, killer)
    game.broadcast(environment(victim).id, victim.name .. " has been slain!")
    
    -- Trigger on_death handler
    if victim.on_death then
        victim:on_death(killer)
    end
    
    -- Stop all combat involving this entity
    Combat.stop(victim)
    
    if victim.class == "player" then
        -- Player death: respawn at bind point
        Combat.player_death(victim, killer)
    else
        -- NPC death: drop loot, schedule respawn
        Combat.npc_death(victim, killer)
    end
end

function Combat.player_death(player, killer)
    -- Move to respawn point
    local respawn = player.metadata.bind_point or game.get_universe().metadata.default_spawn
    game.move_object(player.id, respawn)
    
    -- Restore health
    player.metadata.health = player.metadata.max_health
    game.update_object(player.id, { metadata = player.metadata })
    
    game.send(player.id, "You have died and respawned at your bind point.")
    
    -- Optional: death penalty (XP loss, item drop, etc.)
    if game.get_universe().metadata.death_penalty then
        -- Universe-specific death penalty logic
    end
end

function Combat.npc_death(npc, killer)
    local room = environment(npc)
    
    -- Drop inventory as loot
    for _, item in ipairs(all_inventory(npc)) do
        game.move_object(item.id, room.id)
        game.broadcast(room.id, npc.name .. " drops " .. item.name .. ".")
    end
    
    -- Award XP to killer
    if killer and killer.class == "player" then
        local xp = npc.metadata.xp_value or 10
        killer.metadata.xp = (killer.metadata.xp or 0) + xp
        game.update_object(killer.id, { metadata = killer.metadata })
        game.send(killer.id, "You gain " .. xp .. " experience.")
    end
    
    -- Schedule respawn if configured
    if npc.metadata.respawn_time then
        local spawn_room = npc.metadata.spawn_room or room.id
        local npc_class = npc.class
        local npc_props = npc.metadata.spawn_props or { name = npc.name }
        
        game.call_out(room.id, "spawn_npc", npc.metadata.respawn_time, npc_class, npc_props)
    end
    
    -- Delete the corpse (or create corpse object)
    game.delete_object(npc.id)
end
```

#### Combat Commands

```lua
-- Player initiates combat
function Commands.attack(actor, target_name)
    local room = environment(actor)
    local target = game.present_living(target_name, room.id)
    
    if not target then
        return { success = false, message = "You don't see " .. target_name .. " here." }
    end
    
    if target.id == actor.id then
        return { success = false, message = "You cannot attack yourself." }
    end
    
    return Combat.initiate(actor, target)
end

-- Player flees combat
function Commands.flee(actor)
    if not actor.metadata.in_combat then
        return { success = false, message = "You are not in combat." }
    end
    
    local room = environment(actor)
    local exits = room.metadata.exits or {}
    local directions = {}
    for dir, _ in pairs(exits) do
        table.insert(directions, dir)
    end
    
    if #directions == 0 then
        return { success = false, message = "There's nowhere to flee!" }
    end
    
    -- 50% chance to flee successfully
    if game.random(1, 2) == 1 then
        return { success = false, message = "You fail to escape!" }
    end
    
    Combat.stop(actor)
    local dir = directions[game.random(1, #directions)]
    game.broadcast(room.id, actor.name .. " flees " .. dir .. "!")
    return Commands.go(actor, dir)
end

-- Change target mid-combat
function Commands.target(actor, target_name)
    if not actor.metadata.in_combat then
        return { success = false, message = "You are not in combat." }
    end
    
    local room = environment(actor)
    local target = game.present_living(target_name, room.id)
    
    if not target then
        return { success = false, message = "You don't see " .. target_name .. " here." }
    end
    
    if not Combat.can_attack(actor, target) then
        return { success = false, message = "You cannot attack that target." }
    end
    
    actor.metadata.attacking = target.id
    target.metadata.attackers[actor.id] = true
    game.update_object(actor.id, { metadata = actor.metadata })
    game.update_object(target.id, { metadata = target.metadata })
    
    return { success = true, message = "You are now attacking " .. target.name .. "." }
end
```

#### Damage Types and Resolution

```lua
Combat.DAMAGE_TYPES = {
    "physical", "fire", "cold", "lightning", 
    "poison", "necrotic", "radiant", "psychic"
}

Combat.STATUS_EFFECTS = {
    poisoned = { duration = 10, damage_per_tick = 2, stat_mod = { attack_bonus = -2 } },
    stunned = { duration = 2, can_act = false },
    blinded = { duration = 5, stat_mod = { attack_bonus = -4 } },
    invisible = { duration = 5, stat_mod = { armor_class = 4 } },
    burning = { duration = 3, damage_per_tick = 5, damage_type = "fire" },
    frozen = { duration = 2, stat_mod = { armor_class = -2 }, can_act = false },
}

function Combat.roll(dice_str)  -- "2d6+3" format
    local count, sides, modifier = dice_str:match("(%d+)d(%d+)([+-]?%d*)")
    local total = tonumber(modifier) or 0
    for i = 1, tonumber(count) do 
        total = total + game.random(1, tonumber(sides)) 
    end
    return total
end

function Combat.deal_damage(entity_id, amount, damage_type)
    local entity = game.get_object(entity_id)
    local meta = entity.metadata
    local applied = amount
    local message = nil
    
    -- Immunity: no damage
    if meta.immunities and meta.immunities[damage_type] then
        applied = 0
        message = entity.name .. " is immune to " .. damage_type .. "!"
    -- Resistance: half damage
    elseif meta.resistances and meta.resistances[damage_type] then
        applied = math.floor(amount / 2)
        message = entity.name .. " resists some " .. damage_type .. " damage."
    -- Vulnerability: double damage
    elseif meta.vulnerabilities and meta.vulnerabilities[damage_type] then
        applied = amount * 2
        message = entity.name .. " is vulnerable to " .. damage_type .. "!"
    end
    
    -- Apply damage
    local new_health = math.max(0, (meta.health or 0) - applied)
    meta.health = new_health
    game.update_object(entity_id, { metadata = meta })
    
    -- Trigger on_damage handler
    if entity.on_damage then
        entity:on_damage(applied, damage_type, nil)
    end
    
    if new_health <= 0 then
        Combat.handle_death(entity, nil)
    end
    
    return { applied = applied, original = amount, type = damage_type, message = message }
end

function Combat.attack_extended(attacker, defender, weapon)
    -- Check if attacker can act (status effects)
    if attacker.metadata.statuses then
        for status_name, _ in pairs(attacker.metadata.statuses) do
            local effect = Combat.STATUS_EFFECTS[status_name]
            if effect and effect.can_act == false then
                game.send(attacker.id, "You cannot act while " .. status_name .. "!")
                return { hit = false, message = "Cannot act" }
            end
        end
    end
    
    local result = {
        hit = false,
        critical = false,
        roll = 0,
        physical_damage = 0,
        elemental_damage_rolled = 0,
        elemental_damage_applied = 0,
        elemental_type = nil,
        message = ""
    }
    
    -- Apply stat modifiers from status effects
    local attack_bonus = attacker.metadata.attack_bonus or 0
    local defender_ac = defender.metadata.armor_class or 10
    
    if attacker.metadata.statuses then
        for status_name, _ in pairs(attacker.metadata.statuses) do
            local effect = Combat.STATUS_EFFECTS[status_name]
            if effect and effect.stat_mod and effect.stat_mod.attack_bonus then
                attack_bonus = attack_bonus + effect.stat_mod.attack_bonus
            end
        end
    end
    
    if defender.metadata.statuses then
        for status_name, _ in pairs(defender.metadata.statuses) do
            local effect = Combat.STATUS_EFFECTS[status_name]
            if effect and effect.stat_mod and effect.stat_mod.armor_class then
                defender_ac = defender_ac + effect.stat_mod.armor_class
            end
        end
    end
    
    -- Roll to hit
    result.roll = game.random(1, 20)
    local attack_total = result.roll + attack_bonus
    
    result.critical = (result.roll == 20)
    result.hit = result.critical or (result.roll ~= 1 and attack_total >= defender_ac)
    
    if not result.hit then
        result.message = attacker.name .. " misses " .. defender.name .. "!"
        game.broadcast(environment(attacker).id, result.message)
        return result
    end
    
    -- Physical damage
    local dice = weapon.metadata.damage_dice or "1d4"
    result.physical_damage = Combat.roll(dice) + (weapon.metadata.damage_bonus or 0)
    if result.critical then 
        result.physical_damage = result.physical_damage + Combat.roll(dice) 
    end
    
    Combat.deal_damage(defender.id, result.physical_damage, weapon.metadata.damage_type or "physical")
    
    -- Elemental damage (if weapon has it)
    if weapon.metadata.elemental_damage_dice then
        result.elemental_type = weapon.metadata.elemental_damage_type or "fire"
        result.elemental_damage_rolled = Combat.roll(weapon.metadata.elemental_damage_dice)
        local elem_result = Combat.deal_damage(defender.id, result.elemental_damage_rolled, result.elemental_type)
        result.elemental_damage_applied = elem_result.applied
        if elem_result.message then 
            result.message = result.message .. " " .. elem_result.message 
        end
    end
    
    -- Weapon on_hit handler (for special effects)
    if weapon.on_hit then
        result = weapon:on_hit(attacker, defender, result)
    end
    
    local total = result.physical_damage + result.elemental_damage_applied
    result.message = attacker.name .. " hits " .. defender.name .. " for " .. total .. " damage!"
    if result.critical then 
        result.message = "Critical! " .. result.message 
    end
    
    game.broadcast(environment(attacker).id, result.message)
    return result
end

-- Process status effects during heartbeat
function Combat.process_statuses(entity)
    if not entity.metadata.statuses then return end
    
    local to_remove = {}
    
    for status_name, status_data in pairs(entity.metadata.statuses) do
        local effect = Combat.STATUS_EFFECTS[status_name]
        
        -- Apply damage-over-time
        if effect.damage_per_tick then
            Combat.deal_damage(entity.id, effect.damage_per_tick, effect.damage_type or "poison")
        end
        
        -- Decrement duration
        status_data.remaining = status_data.remaining - 1
        if status_data.remaining <= 0 then
            table.insert(to_remove, status_name)
        end
    end
    
    -- Remove expired statuses
    for _, status_name in ipairs(to_remove) do
        entity.metadata.statuses[status_name] = nil
        game.send(entity.id, "You are no longer " .. status_name .. ".")
    end
    
    game.update_object(entity.id, { metadata = entity.metadata })
end
```

### High-Occupancy Rooms (100+ players)

Return aggregated view: `{ aggregated: true, player_count, notable_entities[], chat_mode: "sampled" }`. Client shows crowd indicator instead of individual sprites.

### Game Logic Tests (User Story Validation)

Located at `server/src/lua/stdlib/tests/game_logic_tests.lua`. Run via `cargo test --test test_lua_game_logic`.

These tests validate that player-facing game mechanics work as expected before adding infrastructure complexity (Raft, auth, payments).

**User Story 1: Wizard Creates Region**
- `test_region_creation_basic` - Region with metadata (environment, danger_level)
- `test_region_with_attached_code` - Code stored by hash, attached to region
- `test_region_code_executes_on_enter` - on_enter handler fires when player enters
- `test_region_permission_check_wizard_required` - Only wizard+ can create regions

**User Story 2: Room with Fixed Objects**
- `test_room_creation_in_region` - Builder creates room in assigned region
- `test_fixed_object_creation` - Chair/table with `fixed = true`
- `test_player_cannot_take_fixed_object` - Commands.take() fails for fixed items
- `test_player_cannot_move_fixed_object` - game.move_object() blocked for players
- `test_wizard_can_move_fixed_object` - Wizard bypasses fixed restriction
- `test_fixed_object_can_be_interacted_with` - Can sit on fixed chair (use handler)

**User Story 3: Custom Weapon + Daily Spawner**
- `test_define_custom_weapon_class` - fire_sword class with elemental damage
- `test_create_fire_sword_instance` - Instantiate sword in world
- `test_fire_sword_damage_against_normal_enemy` - Physical + fire damage both apply
- `test_fire_sword_against_fire_immune_enemy` - Fire damage = 0, physical applies
- `test_fire_sword_against_fire_resistant_enemy` - Fire damage halved
- `test_spawner_chest_creates_sword` - Magic chest spawns sword on open
- `test_spawner_chest_respects_cooldown` - Cannot spawn again immediately
- `test_spawner_only_one_sword_per_day` - 24h cooldown enforced

**Combat System Tests**
- `test_combat_multiple_damage_types` - Immunity, resistance, vulnerability
- `test_combat_death_handling` - Entity reaches 0 HP

**Combat Loop Tests (PvM/PvP)**
- `test_combat_initiation_pvm` - Player attacks NPC, both enter combat state
- `test_combat_pvp_disabled` - PvP blocked when universe setting is DISABLED
- `test_combat_pvp_arena_only` - PvP only allowed in rooms with `is_arena = true`
- `test_combat_pvp_flagged` - PvP only between players with `pvp_flagged = true`
- `test_combat_heartbeat_round` - Combat advances via `heart_beat()`, round counter increments
- `test_combat_stop_when_target_dies` - Combat ends when target reaches 0 HP
- `test_combat_flee` - Player can flee combat, exits room
- `test_npc_aggro` - NPC with `aggro_targets` attacks player on sight
- `test_npc_flee_at_low_health` - NPC flees when health drops below `flee_threshold`
- `test_status_effects_in_combat` - Stunned prevents action, effects expire after duration

**Permission Tests**
- `test_player_cannot_create_region` - Access level enforced
- `test_builder_can_create_room_in_assigned_region` - assigned_regions check
- `test_builder_cannot_create_room_in_unassigned_region` - Blocked outside zone

**Class Inheritance Tests**
- `test_class_inheritance_chain` - Verify chain: fire_sword → sword → weapon → item → thing
- `test_property_inheritance` - Properties cascade down inheritance chain
- `test_property_override` - Instance can override inherited defaults
- `test_is_a_query` - `game.is_a(sword, "weapon")` returns true for all ancestors
- `test_handler_inheritance_with_parent` - Handler calls `parent()` to invoke parent handler
- `test_handler_override_without_parent` - Handler can completely replace parent behavior
- `test_deep_inheritance_legendary_sword` - 6-level chain with property merging

## 13. API Specifications

### REST Endpoints

```
POST /api/auth/challenge     -> { challenge, expires }
POST /api/auth/verify        -> { token, account }
GET  /api/universes          -> universe[]
POST /api/universes          -> universe (create)
GET  /api/universes/:id      -> universe details
POST /api/credits/purchase   -> { success, new_balance }
```

### WebSocket

`wss://play.hemimud.com/ws` — Auth via JWT in first message.

## 14. Deployment & Operations

### Local Development

```bash
# From repo root
make dev          # Starts server + client in dev mode
make test         # Runs all tests (server, client, contracts)
./scripts/dev-cluster.sh  # Spin up 3-node local Raft cluster
```

### Docker Compose (3-node cluster)

```yaml
# docker-compose.yml (repo root)
services:
  node1:
    build: ./server
    environment:
      - NODE_ID=1
      - CLUSTER_PEERS=node1:9000,node2:9000,node3:9000
      - VENICE_API_KEY=${VENICE_API_KEY}
  # node2, node3 similar
  
  client:
    build: ./client
    ports:
      - "3000:3000"
```

### Monitoring

- Prometheus + Grafana: Raft metrics, WebSocket connections, Lua execution time, Venice latency
- Alerts: No leader >5s, replication lag >1000 entries, high Lua time, Venice errors

### Backup

Raft provides replication, not offsite backup. Additional: periodic SQLite backup to S3/GCS.

## 15. Implementation Roadmap (TDD)

**Philosophy**: Shortest path to validating Lua game logic. Start single-node, add complexity incrementally. Each phase has integration tests that MUST pass before proceeding.

### Critical Path Summary

```
Phase 1-2 (Days 1-5):   Foundation + Lua Sandbox
Phase 3-4 (Days 6-10):  Object System + Rooms/Movement  
Phase 5-6 (Days 11-16): WebSocket + Combat ← GAME PLAYABLE HERE
Phase 7   (Days 17-19): Persistence
Phase 8   (Days 20-24): Raft Replication ← PRODUCTION-READY HERE
Phase 9+  (Days 25+):   Auth, Payments, Venice, Client
```

**Key insight**: Phases 1-6 produce a fully playable single-node game server. All game logic validation happens BEFORE adding Raft complexity. This means:
- Combat balance can be tuned with fast iteration
- Lua APIs are battle-tested before replication
- Bugs are found in simple environment first

---

### Phase 1: Foundation (Days 1-2)

**Goal**: Rust project compiles, SQLite works, HTTP server runs.

**Tasks**:
- `cargo new hemimud-server`
- Dependencies: `tokio`, `axum`, `rusqlite`, `serde`, `serde_json`
- Create `shared.db` schema (accounts table only)
- Create universe DB schema (objects, rooms, lua_code)
- HTTP endpoint: `GET /health` → `{"status": "ok"}`

**Integration Tests**:
```rust
#[tokio::test]
async fn test_health_endpoint() {
    let app = create_app().await;
    let response = app.get("/health").await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn test_database_creation() {
    let db = Database::new_universe(":memory:").await;
    assert!(db.table_exists("objects"));
    assert!(db.table_exists("rooms"));
    assert!(db.table_exists("lua_code"));
}
```

**Validation Gate**: `cargo test` passes, server starts on port 8080.

---

### Phase 2: Lua Sandbox (Days 3-5)

**Goal**: Execute Lua code safely with metering and resource limits.

**Tasks**:
- Add `mlua` dependency (with `send` feature for async)
- Create `LuaSandbox` struct with instruction limit hook
- Remove dangerous globals: `os`, `io`, `loadfile`, `dofile`, `load`
- Implement `game.random(min, max)` using seeded RNG
- Implement `game.time()` returning Unix ms
- Implement `game.log(level, message)` with rate limiting

**Integration Tests**:
```rust
#[test]
fn test_sandbox_executes_basic_lua() {
    let sandbox = LuaSandbox::new(1_000_000); // 1M instruction limit
    let result: i32 = sandbox.eval("return 2 + 2").unwrap();
    assert_eq!(result, 4);
}

#[test]
fn test_sandbox_blocks_os_access() {
    let sandbox = LuaSandbox::new(1_000_000);
    let result = sandbox.eval("return os.execute('ls')");
    assert!(result.is_err());
}

#[test]
fn test_sandbox_aborts_infinite_loop() {
    let sandbox = LuaSandbox::new(1_000); // Low limit
    let result = sandbox.eval("while true do end");
    assert!(matches!(result, Err(LuaError::InstructionLimit)));
}

#[test]
fn test_game_random_is_deterministic() {
    let sandbox = LuaSandbox::new_with_seed(1_000_000, 12345);
    let a: i32 = sandbox.eval("return game.random(1, 100)").unwrap();
    let sandbox2 = LuaSandbox::new_with_seed(1_000_000, 12345);
    let b: i32 = sandbox2.eval("return game.random(1, 100)").unwrap();
    assert_eq!(a, b);
}
```

**Validation Gate**: All sandbox tests pass. Can execute Lua with predictable resource limits.

---

### Phase 3: Object System (Days 6-8)

**Goal**: CRUD objects in SQLite, expose to Lua.

**Tasks**:
- Implement `MutationCollector` for batching SQL
- Implement `game.create_object(path, class, parent_id, props) -> object` (path-based ID)
- Implement `game.get_object(id) -> table`
- Implement `game.update_object(id, changes)`
- Implement `game.delete_object(id)`
- Implement `game.move_object(id, new_parent_id)`
- Implement `game.get_children(parent_id, filters) -> array`

**Integration Tests**:
```rust
#[tokio::test]
async fn test_create_and_get_object() {
    let (db, sandbox) = setup_test_env().await;
    sandbox.eval(r#"
        local obj = game.create_object("/items/sword", "item", "/rooms/room1", {name = "sword", damage = 10})
        local loaded = game.get_object(obj.id)
        assert(loaded.name == "sword")
        assert(loaded.metadata.damage == 10)
    "#).unwrap();
}

#[tokio::test]
async fn test_move_object() {
    let (db, sandbox) = setup_test_env().await;
    sandbox.eval(r#"
        local sword = game.create_object("/items/sword", "item", "/rooms/room1", {name = "sword"})
        game.move_object(sword.id, "/rooms/room2")
        local obj = game.get_object(sword.id)
        assert(obj.parent_id == "/rooms/room2")
    "#).unwrap();
}

#[tokio::test]
async fn test_get_children_with_filter() {
    let (db, sandbox) = setup_test_env().await;
    sandbox.eval(r#"
        game.create_object("/items/sword", "item", "/rooms/room1", {name = "sword"})
        game.create_object("/items/shield", "item", "/rooms/room1", {name = "shield"})
        game.create_object("/npcs/goblin", "bot", "/rooms/room1", {name = "goblin"})
        local items = game.get_children("/rooms/room1", {class = "item"})
        assert(#items == 2)
    "#).unwrap();
}

#[tokio::test]
async fn test_mutations_are_collected() {
    let (db, sandbox, collector) = setup_test_env_with_collector().await;
    sandbox.eval(r#"
        game.create_object("/items/sword", "item", "/rooms/room1", {name = "sword"})
        game.update_object("/rooms/room1", {description = "A dark room"})
    "#).unwrap();
    let mutations = collector.into_sql();
    assert_eq!(mutations.len(), 2);
    assert!(mutations[0].starts_with("INSERT INTO objects"));
    assert!(mutations[1].starts_with("UPDATE objects"));
}
```

**Validation Gate**: Full object CRUD works via Lua. Mutations collected as SQL strings.

---

### Phase 4: Rooms & Movement (Days 9-10)

**Goal**: Players can move between rooms, see room contents.

**Tasks**:
- Create test universe with 3 connected rooms
- Implement `game.get_exits(room_id) -> table`
- Implement `game.send(target_id, message)` (queues for delivery)
- Implement `game.broadcast(room_id, message)`
- Implement movement command handler in Lua
- Implement look command handler in Lua

**Integration Tests**:
```rust
#[tokio::test]
async fn test_player_can_move_north() {
    let env = setup_test_world().await; // Creates rooms with exits
    env.exec_as_player("player1", r#"
        local player = game.get_object("player1")
        assert(player.parent_id == "room_start")
        
        -- Move north
        local exits = game.get_exits(player.parent_id)
        assert(exits.north == "room_north")
        game.move_object(player.id, exits.north)
        
        player = game.get_object("player1")
        assert(player.parent_id == "room_north")
    "#).unwrap();
}

#[tokio::test]
async fn test_broadcast_reaches_room_occupants() {
    let env = setup_test_world().await;
    env.exec_as_player("player1", r#"
        game.broadcast("room_start", "Hello everyone!")
    "#).unwrap();
    
    let messages = env.get_pending_messages("player2"); // player2 in same room
    assert!(messages.contains(&"Hello everyone!".to_string()));
    
    let messages = env.get_pending_messages("player3"); // player3 in different room
    assert!(messages.is_empty());
}

#[tokio::test]
async fn test_look_command() {
    let env = setup_test_world().await;
    let output = env.exec_command("player1", "look").await;
    assert!(output.contains("room_start")); // room name
    assert!(output.contains("player2")); // other player in room
}
```

**Validation Gate**: Player movement and room interaction works entirely in Lua.

---

### Phase 5: WebSocket & Commands (Days 11-13)

**Goal**: Players connect via WebSocket, send commands, receive responses.

**Tasks**:
- Add `tokio-tungstenite` for WebSocket
- Implement connection handler with session state
- Implement command parser (splits input, routes to Lua handlers)
- Implement `game.register_command(name, handler)`
- Wire up message delivery to WebSocket send
- Simple auth: hardcoded test tokens (real auth comes later)

**Integration Tests**:
```rust
#[tokio::test]
async fn test_websocket_connection() {
    let server = start_test_server().await;
    let (mut ws, _) = connect_async(&server.ws_url()).await.unwrap();
    
    ws.send(Message::Text(r#"{"type":"auth","token":"test_player1"}"#.into())).await.unwrap();
    let response = ws.next().await.unwrap().unwrap();
    assert!(response.to_text().unwrap().contains("auth_ok"));
}

#[tokio::test]
async fn test_command_execution_via_websocket() {
    let server = start_test_server().await;
    let mut ws = connect_and_auth(&server, "player1").await;
    
    ws.send(Message::Text(r#"{"type":"command","text":"look"}"#.into())).await.unwrap();
    let response = ws.next().await.unwrap().unwrap();
    let msg: ServerMessage = serde_json::from_str(response.to_text().unwrap()).unwrap();
    assert!(matches!(msg, ServerMessage::RoomState { .. }));
}

#[tokio::test]
async fn test_movement_updates_both_rooms() {
    let server = start_test_server().await;
    let mut ws1 = connect_and_auth(&server, "player1").await; // in room_start
    let mut ws2 = connect_and_auth(&server, "player2").await; // in room_start
    
    // player1 moves north
    ws1.send(Message::Text(r#"{"type":"command","text":"north"}"#.into())).await.unwrap();
    
    // player2 should see departure message
    let msg = ws2.next().await.unwrap().unwrap();
    assert!(msg.to_text().unwrap().contains("player1 leaves north"));
}
```

**Validation Gate**: Full client-server loop works. Commands execute Lua, responses delivered via WebSocket.

---

### Phase 6: Combat System (Days 14-16)

**Goal**: D&D-style combat works entirely in Lua.

**Tasks**:
- Implement combat.lua standard library (roll, attack, damage, death)
- Implement `attack <target>` command
- Implement health, armor_class, attack_bonus stats
- Implement death handling (respawn or corpse)
- Implement status effects with expiration

**Integration Tests**:
```rust
#[tokio::test]
async fn test_attack_hits_and_damages() {
    let env = setup_combat_test().await;
    // Seed RNG for deterministic roll (will hit)
    env.set_rng_seed(12345); 
    
    env.exec_command("player1", "attack goblin").await;
    
    let goblin = env.get_object("goblin1");
    assert!(goblin.get_stat("health") < 10); // Started with 10 HP
}

#[tokio::test]
async fn test_attack_can_miss() {
    let env = setup_combat_test().await;
    env.set_rng_seed(99999); // Seed that produces low roll
    
    let initial_health = env.get_object("goblin1").get_stat("health");
    env.exec_command("player1", "attack goblin").await;
    
    let goblin = env.get_object("goblin1");
    assert_eq!(goblin.get_stat("health"), initial_health); // No damage
}

#[tokio::test]
async fn test_death_removes_from_combat() {
    let env = setup_combat_test().await;
    env.set_object_stat("goblin1", "health", 1);
    env.set_rng_seed(12345); // Guaranteed hit
    
    env.exec_command("player1", "attack goblin").await;
    
    let goblin = env.get_object("goblin1");
    assert!(goblin.get_stat("health") <= 0);
    // Goblin should be dead/removed
}

#[tokio::test]
async fn test_status_effect_expires() {
    let env = setup_combat_test().await;
    env.exec_as_player("player1", r#"
        Combat.apply_status(game.get_object("player1"), "poisoned", 2.0)
    "#).await;
    
    assert!(env.has_status("player1", "poisoned"));
    
    // Advance time by 3 seconds
    env.advance_time(Duration::from_secs(3)).await;
    env.exec_as_player("player1", r#"
        Combat.check_status_effects(game.get_object("player1"))
    "#).await;
    
    assert!(!env.has_status("player1", "poisoned"));
}
```

**Validation Gate**: Full combat loop works in Lua. Can validate game balance at this point.

---

### Phase 7: Persistence & Recovery (Days 17-19)

**Goal**: Server restarts preserve state. Single-node durability.

**Tasks**:
- Implement proper SQLite WAL mode
- Implement server shutdown with clean DB close
- Implement server startup loading existing universe
- Implement scheduled tasks persistence (timers survive restart)

**Integration Tests**:
```rust
#[tokio::test]
async fn test_state_survives_restart() {
    let db_path = temp_db_path();
    
    // First server instance
    {
        let server = start_server_with_db(&db_path).await;
        let mut ws = connect_and_auth(&server, "player1").await;
        ws.send(cmd("north")).await.unwrap(); // Move player
        server.shutdown().await;
    }
    
    // Second server instance
    {
        let server = start_server_with_db(&db_path).await;
        let player = server.get_object("player1");
        assert_eq!(player.parent_id, "room_north"); // Position preserved
    }
}

#[tokio::test]
async fn test_bot_timer_survives_restart() {
    let db_path = temp_db_path();
    
    {
        let server = start_server_with_db(&db_path).await;
        // Create bot with 60s think interval
        server.create_bot("guard1", "room_start", Duration::from_secs(60)).await;
        server.shutdown().await;
    }
    
    {
        let server = start_server_with_db(&db_path).await;
        // Bot should resume its timer
        assert!(server.has_active_timer("guard1"));
    }
}
```

**Validation Gate**: Server restart preserves all game state.

---

### Phase 8: Raft Replication (Days 20-24)

**Goal**: Multi-node cluster with leader election and log replication.

**Tasks**:
- Add `openraft` dependency
- Implement `RaftStateMachine` applying `GameLogEntry`
- Implement snapshot creation (SQLite backup)
- Implement snapshot transfer
- Modify write path: collect mutations → Raft propose → apply on commit
- Implement leader-only Lua execution check
- Implement follower read-only mode

**Integration Tests**:
```rust
#[tokio::test]
async fn test_three_node_cluster_forms() {
    let cluster = harness::spawn_cluster(3).await;
    cluster.wait_for_leader(Duration::from_secs(5)).await;
    assert!(cluster.has_leader());
}

#[tokio::test]
async fn test_writes_replicate_to_followers() {
    let cluster = harness::spawn_cluster(3).await;
    let leader = cluster.leader().await;
    
    // Write via leader
    leader.exec_command("player1", "north").await;
    
    // Wait for replication
    cluster.wait_for_sync(Duration::from_secs(2)).await;
    
    // Verify on followers
    for follower in cluster.followers() {
        let player = follower.get_object("player1");
        assert_eq!(player.parent_id, "room_north");
    }
}

#[tokio::test]
async fn test_leader_failure_triggers_election() {
    let cluster = harness::spawn_cluster(3).await;
    let old_leader = cluster.leader_id().await;
    
    cluster.kill_node(old_leader).await;
    cluster.wait_for_leader(Duration::from_secs(5)).await;
    
    let new_leader = cluster.leader_id().await;
    assert_ne!(old_leader, new_leader);
}

#[tokio::test]
async fn test_follower_rejects_write() {
    let cluster = harness::spawn_cluster(3).await;
    let follower = cluster.followers().next().unwrap();
    
    let result = follower.exec_command("player1", "north").await;
    assert!(matches!(result, Err(Error::NotLeader { .. })));
}

#[tokio::test]
async fn test_snapshot_recovery() {
    let cluster = harness::spawn_cluster(3).await;
    
    // Generate enough writes to trigger snapshot
    for _ in 0..1000 {
        cluster.leader().await.exec_command("player1", "look").await;
    }
    
    // Add new node
    let new_node = cluster.add_node().await;
    new_node.wait_for_sync(Duration::from_secs(10)).await;
    
    // Verify new node has correct state
    let player = new_node.get_object("player1");
    assert!(player.is_some());
}
```

**Validation Gate**: 3-node cluster survives leader failure. State replicates correctly.

---

### Phase 9: Authentication (Days 25-27)

**Goal**: Wallet-based auth with JWT sessions.

**Tasks**:
- Implement challenge-response: server sends nonce, client signs with wallet
- Implement signature verification (ethers-rs or alloy)
- Implement JWT token generation and validation
- Implement account creation on first login
- Replace test tokens with real auth flow

**Integration Tests**:
```rust
#[tokio::test]
async fn test_wallet_auth_flow() {
    let server = start_test_server().await;
    let wallet = LocalWallet::random();
    
    // Get challenge
    let challenge = server.get_challenge(wallet.address()).await;
    
    // Sign challenge
    let signature = wallet.sign_message(&challenge.message).await.unwrap();
    
    // Verify and get token
    let auth = server.verify_signature(wallet.address(), &signature).await.unwrap();
    assert!(!auth.token.is_empty());
    
    // Use token for WebSocket
    let mut ws = connect_async(&server.ws_url()).await.unwrap().0;
    ws.send(Message::Text(format!(r#"{{"type":"auth","token":"{}"}}"#, auth.token))).await.unwrap();
    let response = ws.next().await.unwrap().unwrap();
    assert!(response.to_text().unwrap().contains("auth_ok"));
}

#[tokio::test]
async fn test_invalid_signature_rejected() {
    let server = start_test_server().await;
    let wallet = LocalWallet::random();
    let other_wallet = LocalWallet::random();
    
    let challenge = server.get_challenge(wallet.address()).await;
    let bad_signature = other_wallet.sign_message(&challenge.message).await.unwrap();
    
    let result = server.verify_signature(wallet.address(), &bad_signature).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_account_created_on_first_login() {
    let server = start_test_server().await;
    let wallet = LocalWallet::random();
    
    assert!(server.get_account(wallet.address()).await.is_none());
    
    // Complete auth flow
    server.full_auth_flow(&wallet).await.unwrap();
    
    assert!(server.get_account(wallet.address()).await.is_some());
}
```

**Validation Gate**: Players authenticate with real wallet signatures.

---

### Phase 10: Payment Integration (Days 28-30)

**Goal**: Credits can be purchased via Payment Hub.

**Tasks**:
- Integrate with Hemi Payment Hub contract (read-only for balance check)
- Implement credit purchase endpoint
- Server calls `hub.pull()` on behalf of universe vendor
- On success: add credits via Raft log entry
- Implement credit deduction for game actions

**Integration Tests** (requires testnet or mock):
```rust
#[tokio::test]
async fn test_credit_purchase_flow() {
    let server = start_test_server_with_mock_hub().await;
    let wallet = test_wallet_with_balance();
    
    // Set budget for our vendor
    server.mock_hub.set_budget(wallet.address(), server.vendor_wallet(), 100_000000);
    
    // Purchase credits
    let result = server.purchase_credits(&wallet, 10_000000).await; // $10
    assert!(result.is_ok());
    
    // Verify credits added
    let credits = server.get_credits(wallet.address(), "universe1").await;
    assert_eq!(credits, 1000); // $10 = 1000 credits at default rate
}

#[tokio::test]
async fn test_credit_deduction_on_action() {
    let server = start_test_server().await;
    server.grant_credits("player1", 100).await;
    
    // Action that costs credits (e.g., create object)
    server.exec_command("player1", "create sword").await.unwrap();
    
    let credits = server.get_credits("player1").await;
    assert!(credits < 100); // Credits deducted
}

#[tokio::test]
async fn test_insufficient_credits_blocks_action() {
    let server = start_test_server().await;
    server.grant_credits("player1", 1).await; // Not enough
    
    let result = server.exec_command("player1", "create sword").await;
    assert!(matches!(result, Err(Error::InsufficientCredits { .. })));
}
```

**Validation Gate**: Full credit purchase and consumption flow works.

---

### Phase 11: Venice AI Integration (Days 31-33)

**Goal**: LLM chat and image generation via Venice API.

**Tasks**:
- Implement `VeniceClient` (OpenAI-compatible HTTP client)
- Implement `game.llm_chat({tier, messages})`
- Implement `game.llm_image({prompt, size})`
- Implement two-step room image generation
- Implement credit charging for AI calls
- Implement rate limiting per bot

**Integration Tests** (requires API key or mock):
```rust
#[tokio::test]
async fn test_llm_chat_basic() {
    let server = start_test_server_with_mock_venice().await;
    server.mock_venice.set_response("Hello, I am a test response.");
    
    let result = server.exec_as_player("player1", r#"
        local response = game.llm_chat({
            tier = "fast",
            messages = {{role = "user", content = "Say hello"}}
        })
        return response
    "#).await.unwrap();
    
    assert!(result.contains("test response"));
}

#[tokio::test]
async fn test_llm_chat_charges_credits() {
    let server = start_test_server_with_mock_venice().await;
    server.grant_credits("player1", 100).await;
    let initial = server.get_credits("player1").await;
    
    server.exec_as_player("player1", r#"
        game.llm_chat({tier = "fast", messages = {{role = "user", content = "Hi"}}})
    "#).await.unwrap();
    
    let after = server.get_credits("player1").await;
    assert!(after < initial); // Credits deducted
}

#[tokio::test]
async fn test_room_image_generation() {
    let server = start_test_server_with_mock_venice().await;
    server.mock_venice.set_image_url("https://example.com/room.png");
    
    let url = server.generate_room_image("room_start").await.unwrap();
    assert_eq!(url, "https://example.com/room.png");
    
    // Verify two API calls were made (prompt generation + image generation)
    assert_eq!(server.mock_venice.call_count(), 2);
}

#[tokio::test]
async fn test_bot_rate_limiting() {
    let server = start_test_server_with_mock_venice().await;
    
    // Bot makes 60 calls (at limit)
    for _ in 0..60 {
        server.exec_as_bot("bot1", r#"
            game.llm_chat({tier = "fast", messages = {{role = "user", content = "Hi"}}})
        "#).await.unwrap();
    }
    
    // 61st call should fail
    let result = server.exec_as_bot("bot1", r#"
        game.llm_chat({tier = "fast", messages = {{role = "user", content = "Hi"}}})
    "#).await;
    
    assert!(matches!(result, Err(Error::RateLimited { .. })));
}
```

**Validation Gate**: AI features work with proper credit charging and rate limiting.

---

### Phase 12: Client Application (Days 34-40)

**Goal**: Playable browser client.

**Tasks**:
- Vite + React + TypeScript setup
- Wallet connection (wagmi + viem)
- WebSocket connection manager
- Room display with canvas rendering
- Command input and history
- Chat/event log display
- Credit balance display
- Payment Hub deposit/budget UI

**Integration Tests** (Playwright/Cypress):
```typescript
test('player can connect and move', async ({ page }) => {
  await page.goto('/');
  await connectWallet(page, testWallet);
  await page.waitForSelector('[data-testid="room-name"]');
  
  expect(await page.textContent('[data-testid="room-name"]')).toBe('Starting Room');
  
  await page.fill('[data-testid="command-input"]', 'north');
  await page.press('[data-testid="command-input"]', 'Enter');
  
  await page.waitForSelector('[data-testid="room-name"]:has-text("North Room")');
});

test('player can purchase credits', async ({ page }) => {
  await page.goto('/');
  await connectWallet(page, testWalletWithBalance);
  
  await page.click('[data-testid="buy-credits"]');
  await page.click('[data-testid="package-1000"]');
  await page.click('[data-testid="confirm-purchase"]');
  
  // Approve transaction in wallet mock
  await approveTransaction(page);
  
  await page.waitForSelector('[data-testid="credit-balance"]:has-text("1000")');
});
```

**Validation Gate**: Full game playable in browser.

---

### Test Infrastructure Requirements

**Test Utilities** (in `server/tests/`):
```
server/tests/
├── harness/
│   ├── mod.rs
│   ├── server.rs      # spawn_server(), spawn_server_with_db()
│   └── cluster.rs     # spawn_cluster()
├── mocks/
│   ├── mod.rs
│   ├── venice.rs      # MockVenice HTTP server
│   └── payment_hub.rs # In-memory contract simulation
├── fixtures/
│   ├── mod.rs
│   ├── world.rs       # setup_test_world() - rooms, exits, players
│   ├── combat.rs      # setup_combat_test() - combat-ready entities
│   └── wallets.rs     # Pre-funded test wallets
└── common.rs          # setup_test_env(), connect_and_auth()
```

**E2E Tests** (in `client/tests/e2e/`):
- `auth.spec.ts` - Wallet connection, login flow
- `gameplay.spec.ts` - Movement, combat, chat
- `payments.spec.ts` - Credit purchase flow

**CI Pipeline** (`.github/workflows/ci.yml`):
```yaml
jobs:
  server:
    runs-on: ubuntu-latest
    steps:
      - run: cd server && cargo test
      - run: cd server && cargo clippy -- -D warnings
  
  client:
    runs-on: ubuntu-latest
    steps:
      - run: cd client && npm ci && npm test
      - run: cd client && npm run lint
  
  contracts:
    runs-on: ubuntu-latest
    steps:
      - run: cd contracts && forge test
      - run: cd contracts && forge fmt --check
  
  e2e:
    needs: [server, client]
    runs-on: ubuntu-latest
    steps:
      - run: docker-compose up -d
      - run: cd client && npm run test:e2e
```

**Coverage Requirements**:
- Unit tests: 80%+ line coverage
- Integration tests: All public APIs exercised
- E2E tests: Critical user journeys covered

---

### Deferred (Post-MVP)

- Multi-universe support (Phase 13)
- Universe creation/deletion (Phase 14)
- Admin tools and moderation (Phase 15)
- Performance optimization (Phase 16)
- Production deployment (Phase 17)
