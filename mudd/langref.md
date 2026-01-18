# HemiMUD Lua Language Reference

This document covers Lua fundamentals and the complete game scripting API.

## Part 1: Lua Crash Course

### Variables and Types

```lua
-- Variables are dynamically typed
local name = "Sword"           -- string
local damage = 10              -- number (integer)
local weight = 2.5             -- number (float)
local is_magic = true          -- boolean
local nothing = nil            -- nil (absence of value)

-- Global variables (avoid in HemiMUD - use local)
GlobalVar = "visible everywhere"
```

### Tables (Arrays and Objects)

```lua
-- Arrays (1-indexed!)
local items = {"sword", "shield", "potion"}
print(items[1])  -- "sword" (NOT items[0])
print(#items)    -- 3 (length)

-- Dictionaries/Objects
local player = {
    name = "Hero",
    hp = 100,
    level = 5
}
print(player.name)     -- "Hero"
print(player["name"])  -- "Hero" (equivalent)

-- Mixed
local room = {
    name = "Dungeon Entrance",
    exits = {north = "room_002", south = "room_001"},
    items = {"torch", "key"}
}
```

### Functions

```lua
-- Function definition
local function greet(name)
    return "Hello, " .. name .. "!"
end

-- Multiple return values
local function get_stats()
    return 100, 50  -- hp, mp
end
local hp, mp = get_stats()

-- Anonymous functions
local double = function(x) return x * 2 end

-- Functions as table values (methods)
local sword = {
    name = "Longsword",
    on_use = function(args)
        return "You swing the sword!"
    end
}
```

### Control Flow

```lua
-- if/elseif/else
if hp <= 0 then
    print("Dead!")
elseif hp < 20 then
    print("Critical!")
else
    print("Healthy")
end

-- for loops
for i = 1, 10 do          -- 1 to 10 inclusive
    print(i)
end

for i = 10, 1, -1 do      -- countdown
    print(i)
end

-- pairs (dictionaries) and ipairs (arrays)
for key, value in pairs(player) do
    print(key .. " = " .. tostring(value))
end

for index, item in ipairs(items) do
    print(index .. ": " .. item)
end

-- while loop
local count = 0
while count < 5 do
    count = count + 1
end
```

### String Operations

```lua
local s = "Hello World"
print(string.upper(s))           -- "HELLO WORLD"
print(string.lower(s))           -- "hello world"
print(string.sub(s, 1, 5))       -- "Hello"
print(string.len(s))             -- 11
print(string.find(s, "World"))   -- 7, 11
print(string.gsub(s, "World", "Lua"))  -- "Hello Lua"

-- Concatenation
local greeting = "Hello" .. " " .. "World"

-- Format strings
local msg = string.format("HP: %d/%d", 50, 100)
```

### Math Operations

```lua
print(math.abs(-5))      -- 5
print(math.floor(3.7))   -- 3
print(math.ceil(3.2))    -- 4
print(math.max(1, 5, 3)) -- 5
print(math.min(1, 5, 3)) -- 1
print(math.sqrt(16))     -- 4
print(math.random(1, 6)) -- random 1-6
```

### Common Gotchas

1. **Arrays are 1-indexed**: `items[1]` is the first element, not `items[0]`
2. **nil vs false**: Both are falsy, but `nil` means "no value" while `false` is a boolean
3. **String concatenation uses `..`**, not `+`
4. **No increment operator**: Use `x = x + 1`, not `x++`
5. **Equality is `==`**, assignment is `=`
6. **Not equal is `~=`**, not `!=`

---

## Part 2: External Resources

- [Official Lua 5.4 Manual](https://www.lua.org/manual/5.4/)
- [Learn Lua in Y Minutes](https://learnxinyminutes.com/docs/lua/)
- [Lua Users Wiki](http://lua-users.org/wiki/)
- [Programming in Lua (book)](https://www.lua.org/pil/)

---

## Part 3: Game API Reference

All game functions are accessed via the `game` table.

### Object CRUD Operations

#### `game.create_object(path, class, parent_id, props)`

Create a new object in the database with a path-based ID.

```lua
-- Create a sword in a room
local sword = game.create_object("/items/iron-sword", "weapon", "/rooms/armory", {
    name = "Iron Sword",
    description = "A sturdy iron blade",
    damage_dice = "1d8",
    damage_type = "physical"
})
print(sword.id)  -- "/items/iron-sword"
```

**Parameters:**
- `path` (string): Path-based ID (e.g., "/items/sword", "/rooms/tavern"). Must start with `/`, use lowercase, segments `[a-z][a-z0-9-]*`
- `class` (string): Class name (e.g., "item", "weapon", "npc")
- `parent_id` (string|nil): Container object ID (room, player inventory)
- `props` (table|nil): Property overrides

**Returns:** Object table with `id`, `class`, `parent_id`, `name`, `description`, `metadata`. On path validation error, returns `{error = "message"}`

---

#### `game.get_object(id)`

Fetch an object by ID.

```lua
local obj = game.get_object("uuid-here")
if obj then
    print(obj.name)
    print(obj.class)
    print(obj.metadata.hp)  -- Extra properties in metadata
end
```

**Returns:** Object table or `nil` if not found

---

#### `game.update_object(id, changes)`

Update object properties.

```lua
-- Reduce HP
game.update_object(npc_id, {hp = 50})

-- Update multiple properties
game.update_object(item_id, {
    name = "Enchanted Sword",
    damage_dice = "2d6"
})
```

**Returns:** `true` if updated, `false` if object not found

---

#### `game.delete_object(id)`

Delete an object from the database.

```lua
local deleted = game.delete_object("item-uuid")
```

**Returns:** `true` if deleted, `false` if not found

---

#### `game.move_object(id, new_parent_id)`

Move an object to a new container.

```lua
-- Move item to player inventory
game.move_object(sword_id, player_id)

-- Move to room (drop)
game.move_object(sword_id, room_id)

-- Remove from container (orphan)
game.move_object(sword_id, nil)
```

**Returns:** `true` on success

---

#### `game.clone_object(id, new_path, new_parent_id)`

Create a copy of an object with a new path-based ID.

```lua
local copy = game.clone_object("/items/template-sword", "/items/player-sword", "/rooms/chest")
print(copy.id)  -- "/items/player-sword"
```

**Parameters:**
- `id` (string): Source object ID to clone
- `new_path` (string): Path-based ID for the clone (e.g., "/items/sword-copy")
- `new_parent_id` (string|nil): Container for the clone

**Returns:** New object table, `nil` if source not found, or `{error = "message"}` on path validation error

---

### Code Storage

#### `game.store_code(source)`

Store Lua source code (content-addressed by SHA256 hash).

```lua
local code = [[
return {
    on_use = function(args)
        game.send(args.actor_id, "You drink the potion!")
        game.update_object(args.object_id, {used = true})
        return true
    end
}
]]
local hash = game.store_code(code)
```

**Returns:** SHA256 hash string

---

#### `game.get_code(hash)`

Retrieve stored code by hash.

```lua
local source = game.get_code("abc123...")
if source then
    -- Execute or inspect
end
```

**Returns:** Source string or `nil`

---

### Class System

#### `game.define_class(name, definition)`

Define a custom class with inheritance.

```lua
game.define_class("fire_sword", {
    parent = "weapon",
    properties = {
        fire_damage = {type = "string", default = "1d6"},
        burn_chance = {type = "number", default = 25}
    }
})
```

**Definition fields:**
- `parent` (string|nil): Parent class name
- `properties` (table): Property definitions with `type` and `default`

---

#### `game.get_class(name)`

Get a class definition.

```lua
local cls = game.get_class("weapon")
print(cls.name)        -- "weapon"
print(cls.parent)      -- "item"
print(cls.handlers)    -- {"on_move", "on_use", ...}
```

**Returns:** Class table or `nil`

---

#### `game.is_a(obj_id, class_name)`

Check if object is instance of class (including inheritance).

```lua
if game.is_a(item_id, "weapon") then
    print("It's a weapon!")
end

if game.is_a(player_id, "living") then
    print("It can fight!")
end
```

**Returns:** `true` or `false`

---

#### `game.get_class_chain(class_name)`

Get inheritance chain from child to root.

```lua
local chain = game.get_class_chain("fire_sword")
-- {"fire_sword", "weapon", "item", "thing"}
```

**Returns:** Array of class names

---

#### `parent(class_name, handler_name, args)`

Call parent class handler (global function, not on game table).

```lua
-- In a fire_sword's on_use handler:
return {
    on_use = function(args)
        -- Do fire-specific stuff first
        game.send(args.actor_id, "The blade bursts into flames!")

        -- Then call parent weapon's on_use
        return parent("fire_sword", "on_use", args)
    end
}
```

---

### Query Functions

#### `game.environment(obj_id)`

Get the containing object (room, container, player).

```lua
local container = game.environment(item_id)
if container then
    print("Item is in: " .. container.name)
end
```

**Returns:** Object table or `nil`

---

#### `game.all_inventory(obj_id)`

Get all objects contained by an object.

```lua
local contents = game.all_inventory(room_id)
for _, obj in ipairs(contents) do
    print(obj.name)
end
```

**Returns:** Array of object tables

---

#### `game.present(name, env_id)`

Find object by name within a container.

```lua
local sword = game.present("sword", room_id)
if sword then
    game.send(player_id, "You see a " .. sword.name)
end
```

**Returns:** Object table or `nil`

---

#### `game.get_living_in(env_id)`

Get all living entities (players, NPCs) in a location.

```lua
local fighters = game.get_living_in(room_id)
for _, npc in ipairs(fighters) do
    if npc.class == "npc" and npc.metadata.aggro then
        -- Start combat!
    end
end
```

**Returns:** Array of living object tables

---

#### `game.get_children(parent_id, filter)`

Get objects with optional class filter.

```lua
-- All items in a room
local items = game.get_children(room_id, {class = "item"})

-- All objects (no filter)
local all = game.get_children(room_id)
```

**Returns:** Array of object tables

---

### Action System

#### `game.add_action(verb, object_id, method)`

Register a contextual verb for the current room.

```lua
-- Make "pull lever" trigger lever's do_pull method
game.add_action("pull", lever_id, "do_pull")
```

Players can then type `pull lever` to trigger the action.

---

#### `game.remove_action(verb, object_id)`

Remove a registered action.

```lua
game.remove_action("pull", lever_id)
```

---

### Messaging

#### `game.send(target_id, message)`

Send a private message to a player.

```lua
game.send(player_id, "You feel a chill run down your spine.")
game.send(player_id, "The sword glows with an eerie light.")
```

---

#### `game.broadcast(room_id, message)`

Send a message to all players in a room.

```lua
game.broadcast(room_id, "The ground shakes violently!")
game.broadcast(room_id, player_name .. " has entered the room.")
```

---

#### `game.broadcast_region(region_id, message)`

Send a message to all players in a region.

```lua
game.broadcast_region(dungeon_region_id, "A distant roar echoes through the halls.")
```

---

### Permission System

#### `game.set_actor(actor_id)`

Set the current actor for permission checks.

```lua
game.set_actor("player-uuid")
```

---

#### `game.get_actor()`

Get the current actor ID.

```lua
local actor = game.get_actor()
```

---

#### `game.check_permission(action, target_id, is_fixed, region_id)`

Check if current actor can perform an action.

```lua
local result = game.check_permission("modify", obj_id, false, nil)
if result.allowed then
    -- Proceed with modification
else
    game.send(actor_id, "Permission denied: " .. result.error)
end
```

**Actions:** `"read"`, `"modify"`, `"move"`, `"delete"`, `"create"`, `"execute"`, `"admin_config"`, `"grant_credits"`

**Returns:** `{allowed = true}` or `{allowed = false, error = "reason"}`

---

#### `game.get_access_level(account_id)`

Get a user's access level.

```lua
local level = game.get_access_level(account_id)
-- "player", "builder", "wizard", "admin", or "owner"
```

---

#### `game.set_access_level(account_id, level)`

Set a user's access level (admin only).

```lua
game.set_access_level("user-uuid", "builder")
```

**Levels:** `"player"`, `"builder"`, `"wizard"`, `"admin"`, `"owner"`

---

#### `game.assign_region(account_id, region_id)`

Assign a region to a builder.

```lua
game.assign_region(builder_id, "dungeon_region")
```

---

#### `game.unassign_region(account_id, region_id)`

Remove region assignment from a builder.

```lua
game.unassign_region(builder_id, "dungeon_region")
```

---

### Timer System

#### `game.call_out(delay_secs, method, args)`

Schedule a one-shot timer callback.

```lua
-- Explode after 5 seconds
local timer_id = game.call_out(5.0, "do_explode", "boom!")

-- In the object's code:
return {
    do_explode = function(args)
        game.broadcast(room_id, "BOOM! The bomb explodes!")
        game.delete_object(bomb_id)
    end
}
```

**Returns:** Timer ID for cancellation

---

#### `game.remove_call_out(timer_id)`

Cancel a scheduled timer.

```lua
local removed = game.remove_call_out(timer_id)
```

**Returns:** `true` if removed, `false` if not found

---

#### `game.set_heart_beat(interval_ms)`

Set recurring heartbeat for current object.

```lua
-- Tick every 2 seconds
game.set_heart_beat(2000)

-- In object code:
return {
    heart_beat = function(args)
        -- Called every 2 seconds
        local npc = game.get_object(args.object_id)
        if npc.metadata.in_combat then
            -- Do combat AI
        else
            -- Do idle behavior
        end
    end
}
```

---

#### `game.remove_heart_beat()`

Stop heartbeat for current object.

```lua
game.remove_heart_beat()
```

---

### Credits (Economy)

#### `game.get_credits()`

Get current player's credit balance.

```lua
local balance = game.get_credits()
game.send(player_id, "You have " .. balance .. " credits.")
```

---

#### `game.deduct_credits(amount, reason)`

Deduct credits from current player.

```lua
local cost = 100
if game.deduct_credits(cost, "Bought healing potion") then
    -- Give the potion
else
    game.send(player_id, "Not enough credits!")
end
```

**Returns:** `true` if successful, `false` if insufficient funds

---

#### `game.admin_grant_credits(account_id, amount)`

Grant credits to a player (wizard+ only).

```lua
game.admin_grant_credits(player_id, 1000)
```

**Returns:** `true` if granted, `false` if permission denied

---

### Venice AI Integration

#### `game.llm_chat(messages, tier)`

Send chat completion request to Venice AI.

```lua
local response = game.llm_chat({
    {role = "system", content = "You are a wise wizard NPC."},
    {role = "user", content = "What is the meaning of life?"}
}, "balanced")

if type(response) == "string" then
    game.send(player_id, "The wizard says: " .. response)
else
    game.send(player_id, "The wizard seems confused...")
end
```

**Tiers:** `"fast"`, `"balanced"`, `"quality"`

**Returns:** Response string or `{error = "message"}`

---

#### `game.llm_image(prompt, style, size)`

Generate an image using Venice AI.

```lua
local url = game.llm_image(
    "A dark dungeon entrance with torches",
    "realistic",
    "medium"
)

if type(url) == "string" then
    game.update_object(room_id, {image_url = url})
end
```

**Styles:** `"realistic"`, `"anime"`, `"digital"`, `"painterly"`

**Sizes:** `"small"`, `"medium"`, `"large"`

**Returns:** URL string or `{error = "message"}`

---

### Utility Functions

#### `game.time()`

Get current time in milliseconds since epoch.

```lua
local now = game.time()
local last_use = obj.metadata.last_use or 0
if now - last_use < 60000 then
    game.send(player_id, "Item on cooldown!")
end
```

---

#### `game.set_time(time_ms)`

Override current time for testing (wizard+ only).

```lua
game.set_time(1700000000000)  -- Set specific time
game.set_time(0)              -- Return to real time
```

---

#### `game.roll_dice(dice_str)`

Parse and roll dice notation.

```lua
local damage = game.roll_dice("2d6+3")  -- 2 six-sided dice + 3
local check = game.roll_dice("1d20")     -- Single d20
local healing = game.roll_dice("3d8-2")  -- 3d8 minus 2
```

**Format:** `NdM+K` or `NdM-K` where N=count, M=sides, K=modifier

**Returns:** Total roll result

---

#### `game.set_rng_seed(seed)`

Set RNG seed for reproducible testing (wizard+ only).

```lua
game.set_rng_seed(12345)
local roll1 = game.roll_dice("1d20")  -- Always same sequence
```

---

#### `game.use_object(obj_id, actor_id, verb, target_id)`

Invoke an object's handler method.

```lua
local result = game.use_object(potion_id, player_id, "use", nil)
```

**Verb mapping:**
- `"use"` → `on_use`
- `"hit"` → `on_hit`
- `"look"` → `on_look`
- `"init"` → `on_init`

---

#### `game.get_universe()`

Get current universe info.

```lua
local universe = game.get_universe()
print(universe.id)
print(universe.name)
print(universe.owner_id)
print(universe.config)  -- Config table
```

---

#### `game.update_universe(config)`

Update universe config (wizard+ only).

```lua
game.update_universe({
    pvp_enabled = true,
    spawn_room = "new_room_id"
})
```

---

## Part 4: Sandbox Limits

Scripts execute in a secure sandbox with resource limits:

| Resource | Default Limit |
|----------|---------------|
| Instructions | 1,000,000 (wizard eval: 10,000,000) |
| Memory | 64 MB |
| Timeout | 500 ms (wizard eval: 5 seconds) |
| Database queries | 100 per execution |
| Venice API calls | 5 per execution |

### Removed Globals

The following are **not available** in the sandbox:

- `os` - Operating system access
- `io` - File I/O
- `loadfile`, `dofile`, `load`, `loadstring` - Code loading
- `require`, `package` - Module system
- `debug` - Debug library
- `collectgarbage` - GC control

### Available Libraries

- `string` - String manipulation
- `table` - Table operations
- `math` - Math functions
- `utf8` - UTF-8 handling
- `print` - Safe print (does nothing)

---

## Part 5: Base Classes Reference

### thing (root)

The base class for all objects.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `name` | string | `""` | Display name |
| `description` | string | `""` | Description text |

**Handlers:** `on_create`, `on_destroy`, `on_init`

---

### item (extends thing)

Movable objects that can be picked up.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `weight` | number | `0` | Weight in pounds |
| `value` | number | `0` | Value in credits |
| `fixed` | boolean | `false` | If true, cannot be moved |

**Handlers:** `on_move`, `on_use`

---

### living (extends thing)

Entities that can engage in combat.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `hp` | number | `100` | Current hit points |
| `max_hp` | number | `100` | Maximum hit points |
| `attack_bonus` | number | `0` | Bonus to attack rolls |
| `armor_class` | number | `10` | Defense value |
| `in_combat` | boolean | `false` | Currently fighting |

**Handlers:** `heart_beat`, `on_damage`, `on_death`

---

### room (extends thing)

Locations that contain other objects.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `exits` | object | `{}` | Direction → room_id mapping |
| `lighting` | string | `"normal"` | Lighting level |
| `region_id` | string | `null` | Parent region |

**Handlers:** `on_enter`, `on_leave`

---

### region (extends thing)

Groups of rooms with shared properties.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `environment_type` | string | `"dungeon"` | Environment theme |
| `danger_level` | number | `1` | Difficulty rating |
| `ambient_sounds` | array | `[]` | Background sounds |

---

### weapon (extends item)

Items used for dealing damage.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `damage_dice` | string | `"1d6"` | Dice notation |
| `damage_bonus` | number | `0` | Flat damage bonus |
| `damage_type` | string | `"physical"` | Damage type |

**Damage Types:** `physical`, `fire`, `ice`, `lightning`, `poison`, `necrotic`, `radiant`, `psychic`

---

### armor (extends item)

Protective equipment.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `armor_value` | number | `0` | AC bonus |
| `slot` | string | `"body"` | Equipment slot |

---

### container (extends item)

Objects that hold other objects.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `capacity` | number | `10` | Max items |
| `locked` | boolean | `false` | Requires key |

---

### player (extends living)

Player characters.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `wallet_address` | string | `null` | Crypto wallet |
| `access_level` | string | `"player"` | Permission level |

---

### npc (extends living)

Non-player characters.

**Properties:**
| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `aggro` | boolean | `false` | Auto-attack players |
| `respawn_time` | number | `null` | Respawn delay (ms) |

**Handlers:** `ai_idle_tick`, `ai_combat_tick`
