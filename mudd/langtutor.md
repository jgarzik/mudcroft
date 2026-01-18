# HemiMUD Content Creator Tutorial

A hands-on guide for programmers new to Lua and MUD development.

---

## Getting Started: Universe Initialization

Before players can enter your universe, you must set a **spawn portal** - the room where new players appear. Without this, players see "Universe not initialized" when connecting.

### Step 1: Create Your World

After uploading your universe (via API or ZIP), connect as a wizard and run your initialization script:

```
eval return init()
```

This creates your rooms, NPCs, and items.

### Step 2: Find Your Entrance Room

Use the `goto` command to teleport to a room by its ID:

```
goto <room_id>
```

If you don't know the room ID, use Lua to find it:

```lua
eval local rooms = game.get_children(nil, {class = "room"}); for _, r in ipairs(rooms) do print(r.id .. " = " .. r.name) end
```

### Step 3: Set the Portal

Once you're in the room where players should spawn, run:

```
setportal
```

Or specify a room ID directly:

```
setportal <room_id>
```

You'll see confirmation: `Portal set to: Town Square (uuid-here)`

### Wizard Commands Reference

| Command | Description |
|---------|-------------|
| `goto <room_id>` | Teleport to any room by ID (wizard+) |
| `setportal` | Set spawn portal to current room (wizard+) |
| `setportal <room_id>` | Set spawn portal to specified room (wizard+) |
| `eval <lua>` | Execute Lua code (wizard+) |

Now players connecting to your universe will spawn at the portal room!

---

## Part A: Simple Lessons

### Lesson 1: Your First Room

Rooms are containers that hold players, NPCs, and items. They have exits that connect to other rooms.

```lua
-- Create a simple room with path-based ID
local room = game.create_object("/rooms/town-square", "room", nil, {
    name = "Town Square",
    description = "A bustling square at the heart of town. Cobblestones are worn smooth by centuries of foot traffic. A fountain burbles in the center.",
    exits = {
        north = "/rooms/tavern",
        south = "/rooms/gate",
        east = "/rooms/market"
    },
    lighting = "bright"
})

print("Created room: " .. room.id)  -- "/rooms/town-square"
```

**Key Points:**
- First argument is the path-based ID (e.g., "/rooms/town-square")
- `parent_id` is `nil` for top-level rooms
- `exits` maps directions to destination room paths

**Creating Connected Rooms:**

```lua
-- Create a region first
local region = game.create_object("/regions/town-district", "region", nil, {
    name = "Town District",
    environment_type = "urban",
    danger_level = 1
})

-- Create connected rooms
local square = game.create_object("/rooms/town-square", "room", nil, {
    name = "Town Square",
    description = "The central square.",
    region_id = region.id,
    exits = {}  -- We'll update this after creating other rooms
})

local tavern = game.create_object("/rooms/rusty-tankard", "room", nil, {
    name = "The Rusty Tankard",
    description = "A warm tavern filled with the smell of ale and roasting meat.",
    region_id = region.id,
    exits = {south = square.id}
})

-- Now update the square's exits
game.update_object(square.id, {
    exits = {north = tavern.id}
})
```

---

### Lesson 2: Adding Items

Items are objects that can be picked up, dropped, and used.

```lua
-- Create a basic item in a room (path, class, parent_id, props)
local torch = game.create_object("/items/wooden-torch", "item", room_id, {
    name = "Wooden Torch",
    description = "A simple wooden torch wrapped in oil-soaked rags.",
    weight = 1,
    value = 5,
    fixed = false  -- Can be picked up
})

-- Create a fixed item (scenery)
local fountain = game.create_object("/items/stone-fountain", "item", room_id, {
    name = "Stone Fountain",
    description = "An ornate fountain depicting a dragon. Water flows from its mouth.",
    weight = 1000,
    value = 0,
    fixed = true  -- Cannot be picked up
})

-- Create a valuable item
local gem = game.create_object("/items/ruby", "item", chest_id, {
    name = "Ruby",
    description = "A brilliant red gemstone that catches the light.",
    weight = 0,
    value = 500
})
```

**Property Meanings:**
- `weight`: Affects encumbrance (0 = weightless)
- `value`: Worth in credits
- `fixed`: If true, cannot be moved by players

---

### Lesson 3: NPCs with Combat Stats

NPCs inherit from `living` and have combat-related properties.

```lua
-- Create a basic NPC (path, class, parent_id, props)
local guard = game.create_object("/npcs/town-guard", "npc", room_id, {
    name = "Town Guard",
    description = "A bored-looking guard in chain mail, leaning on his spear.",
    hp = 50,
    max_hp = 50,
    attack_bonus = 3,
    armor_class = 15,
    aggro = false,  -- Won't auto-attack
    respawn_time = nil  -- Won't respawn
})

-- Create an aggressive monster
local goblin = game.create_object("/npcs/goblin-scout", "npc", dungeon_room_id, {
    name = "Goblin Scout",
    description = "A small, green-skinned creature with sharp teeth and beady eyes.",
    hp = 15,
    max_hp = 15,
    attack_bonus = 2,
    armor_class = 12,
    aggro = true,  -- Attacks on sight
    respawn_time = 300000  -- Respawns after 5 minutes
})

-- Create a tough boss
local boss = game.create_object("/npcs/ogre-chieftain", "npc", boss_room_id, {
    name = "Ogre Chieftain",
    description = "A massive ogre wielding a tree trunk as a club.",
    hp = 150,
    max_hp = 150,
    attack_bonus = 7,
    armor_class = 14,
    aggro = true,
    respawn_time = 3600000  -- Respawns after 1 hour
})
```

**Combat Stats:**
- `hp/max_hp`: Health points
- `attack_bonus`: Added to d20 attack rolls
- `armor_class`: Target number to hit (higher = harder)
- `aggro`: If true, attacks players automatically
- `respawn_time`: Milliseconds until respawn (nil = no respawn)

---

### Lesson 4: Custom Classes

Define your own classes that inherit from base classes.

```lua
-- Define a potion class
game.define_class("potion", {
    parent = "item",
    properties = {
        heal_amount = {type = "number", default = 20},
        uses = {type = "number", default = 1}
    }
})

-- Define a magic weapon class
game.define_class("magic_sword", {
    parent = "weapon",
    properties = {
        enchantment = {type = "string", default = "none"},
        bonus_damage = {type = "string", default = "0"}
    }
})

-- Define an undead NPC class
game.define_class("undead", {
    parent = "npc",
    properties = {
        undead_type = {type = "string", default = "zombie"},
        radiant_vulnerability = {type = "boolean", default = true},
        necrotic_immunity = {type = "boolean", default = true}
    }
})

-- Now create instances of custom classes
local health_potion = game.create_object("/items/health-potion", "potion", chest_id, {
    name = "Health Potion",
    description = "A red liquid that shimmers with healing energy.",
    heal_amount = 30,
    uses = 1
})

local flame_sword = game.create_object("/items/flamebrand", "magic_sword", armory_id, {
    name = "Flamebrand",
    description = "A blade wreathed in magical fire.",
    damage_dice = "1d8",
    damage_type = "fire",
    enchantment = "fire",
    bonus_damage = "1d6"
})
```

**Class Inheritance:**
- Child classes inherit all properties from parents
- Override defaults by specifying new values
- Use `game.is_a(obj_id, "parent_class")` to check inheritance

---

### Lesson 5: Behaviors with Handlers

Handlers are Lua functions that respond to events.

```lua
-- Store handler code
local potion_code = [[
return {
    on_use = function(args)
        local potion = game.get_object(args.object_id)
        local heal = potion.metadata.heal_amount or 20

        -- Heal the player
        local player = game.get_object(args.actor_id)
        local new_hp = math.min(
            (player.metadata.hp or 0) + heal,
            player.metadata.max_hp or 100
        )
        game.update_object(args.actor_id, {hp = new_hp})

        -- Send feedback
        game.send(args.actor_id, "You drink the potion and feel refreshed! (+" .. heal .. " HP)")

        -- Consume the potion
        game.delete_object(args.object_id)
        return true
    end
}
]]

local hash = game.store_code(potion_code)

-- Create potion with this code
local potion = game.create_object("/items/health-potion-1", "potion", room_id, {
    name = "Health Potion",
    description = "A bubbling red liquid.",
    heal_amount = 30,
    code_hash = hash
})
```

**Common Handlers:**

| Handler | Triggered When |
|---------|---------------|
| `on_init` | Object is loaded |
| `on_create` | Object is created |
| `on_destroy` | Object is deleted |
| `on_use` | Player uses object |
| `on_move` | Object is moved |
| `on_enter` | Player enters room |
| `on_leave` | Player leaves room |
| `on_damage` | Entity takes damage |
| `on_death` | Entity dies |
| `heart_beat` | Timer tick (recurring) |

**Handler Arguments:**
All handlers receive an `args` table with:
- `object_id`: The object running the handler
- `actor_id`: The player/NPC triggering the action
- `verb`: The action verb
- `target_id`: Optional target object
- `code_hash`: The handler's code hash

---

### Lesson 6: Timers

Schedule one-shot or recurring events.

```lua
-- One-shot timer: exploding barrel
local barrel_code = [[
return {
    on_init = function(args)
        -- Start countdown when barrel is touched
    end,

    start_fuse = function(args)
        game.broadcast(game.environment(args.object_id).id,
            "The barrel begins to hiss and spark!")

        -- Explode in 5 seconds
        game.call_out(5.0, "explode", nil)
    end,

    explode = function(args)
        local barrel = game.get_object(args.object_id)
        local room = game.environment(args.object_id)

        -- Damage everyone in the room
        local victims = game.get_living_in(room.id)
        for _, victim in ipairs(victims) do
            local damage = game.roll_dice("3d6")
            game.update_object(victim.id, {
                hp = (victim.metadata.hp or 0) - damage
            })
            game.send(victim.id, "The explosion hits you for " .. damage .. " damage!")
        end

        game.broadcast(room.id, "BOOM! The barrel explodes!")
        game.delete_object(args.object_id)
    end
}
]]

-- Recurring timer: wandering NPC
local wanderer_code = [[
return {
    on_init = function(args)
        -- Start heartbeat every 30 seconds
        game.set_heart_beat(30000)
    end,

    heart_beat = function(args)
        local npc = game.get_object(args.object_id)
        local room = game.environment(args.object_id)

        -- Get random exit
        local exits = room.metadata.exits or {}
        local directions = {}
        for dir, _ in pairs(exits) do
            table.insert(directions, dir)
        end

        if #directions > 0 then
            local dir = directions[math.random(#directions)]
            local dest_id = exits[dir]

            -- Move to new room
            game.move_object(args.object_id, dest_id)
            game.broadcast(room.id, npc.name .. " wanders " .. dir .. ".")
            game.broadcast(dest_id, npc.name .. " arrives.")
        end
    end
}
]]
```

**Timer Types:**
- `call_out(delay, method, args)`: One-shot, fires once after delay
- `set_heart_beat(interval_ms)`: Recurring, fires repeatedly

---

### Lesson 7: Messaging

Communicate with players and broadcast events.

```lua
-- Private message to one player
game.send(player_id, "You notice a secret passage behind the bookshelf.")

-- Broadcast to everyone in a room
game.broadcast(room_id, "The torches flicker ominously.")

-- Broadcast to entire region
game.broadcast_region(dungeon_region_id, "A distant scream echoes through the halls.")

-- Message with formatting
local damage = 15
game.send(player_id, string.format(
    "The goblin hits you for %d damage! (HP: %d/%d)",
    damage, current_hp, max_hp
))

-- Contextual room messages (exclude actor)
local function announce_action(room_id, actor_id, actor_msg, others_msg)
    game.send(actor_id, actor_msg)
    local people = game.get_living_in(room_id)
    for _, person in ipairs(people) do
        if person.id ~= actor_id then
            game.send(person.id, others_msg)
        end
    end
end

announce_action(room_id, player_id,
    "You draw your sword with a flourish.",
    player_name .. " draws their sword with a flourish.")
```

---

### Lesson 8: Permissions

Control who can modify what.

```lua
-- Check if player can modify an object
local result = game.check_permission("modify", object_id, false, nil)
if not result.allowed then
    game.send(player_id, "Permission denied: " .. result.error)
    return
end

-- Get someone's access level
local level = game.get_access_level(account_id)
if level == "wizard" or level == "admin" or level == "owner" then
    -- Allow special commands
end

-- Set up a builder with region access
game.set_access_level(new_builder_id, "builder")
game.assign_region(new_builder_id, my_region_id)

-- Access level hierarchy (lowest to highest):
-- player < builder < wizard < admin < owner
```

**Permission Levels:**
| Level | Can Do |
|-------|--------|
| Player | Read, interact with non-fixed objects |
| Builder | Create/modify in assigned regions |
| Wizard | Full object control, eval command |
| Admin | Universe config, grant credits |
| Owner | Grant admin access |

---

## Part B: Complete Example - "The Cursed Crypt"

A full Pathfinder-style mini-dungeon ready to upload.

### Overview

- 6 connected rooms forming a crypt
- Custom classes: cursed_weapon, healing_potion, locked_chest, skeleton_warrior
- Boss fight with phases
- Respawning enemies, traps, and treasure

### Custom Classes

```lua
-- Cursed weapon with bonus fire damage
game.define_class("cursed_weapon", {
    parent = "weapon",
    properties = {
        curse_name = {type = "string", default = "unknown"},
        bonus_damage_dice = {type = "string", default = "1d4"},
        bonus_damage_type = {type = "string", default = "fire"}
    }
})

-- Consumable healing potion
game.define_class("healing_potion", {
    parent = "item",
    properties = {
        heal_dice = {type = "string", default = "2d4+2"},
        consumed = {type = "boolean", default = false}
    }
})

-- Locked container requiring a key
game.define_class("locked_chest", {
    parent = "container",
    properties = {
        locked = {type = "boolean", default = true},
        key_id = {type = "string", default = ""},
        loot_spawned = {type = "boolean", default = false}
    }
})

-- Undead skeleton with vulnerabilities
game.define_class("skeleton_warrior", {
    parent = "npc",
    properties = {
        radiant_vulnerable = {type = "boolean", default = true},
        necrotic_immune = {type = "boolean", default = true},
        bludgeon_vulnerable = {type = "boolean", default = true}
    }
})
```

### Region and Rooms

```lua
-- Create the crypt region (path, class, parent_id, props)
local crypt_region = game.create_object("/regions/cursed-crypt", "region", nil, {
    name = "The Cursed Crypt",
    description = "An ancient burial ground corrupted by dark magic.",
    environment_type = "dungeon",
    danger_level = 3,
    ambient_sounds = {"dripping water", "distant moans", "chains rattling"}
})

-- Room 1: Crypt Entrance
local entrance = game.create_object("/rooms/crypt-entrance", "room", nil, {
    name = "Crypt Entrance",
    description = "Stone stairs descend into darkness. The air is thick with the smell of decay and ancient dust. Faded warnings are carved into the archway above.",
    region_id = crypt_region.id,
    lighting = "dim",
    exits = {}
})

-- Room 2: Antechamber
local antechamber = game.create_object("/rooms/burial-antechamber", "room", nil, {
    name = "Burial Antechamber",
    description = "Alcoves line the walls, each containing a skeletal corpse wrapped in rotting cloth. A cold draft whispers through the chamber.",
    region_id = crypt_region.id,
    lighting = "dark",
    exits = {north = entrance.id}
})

-- Room 3: Hall of Bones
local hall = game.create_object("/rooms/hall-of-bones", "room", nil, {
    name = "Hall of Bones",
    description = "The walls are constructed entirely of skulls and bones, arranged in macabre patterns. Two passages lead deeper into the crypt.",
    region_id = crypt_region.id,
    lighting = "dark",
    exits = {east = antechamber.id}
})

-- Room 4: Trap Corridor
local trap_corridor = game.create_object("/rooms/trap-corridor", "room", nil, {
    name = "Trapped Corridor",
    description = "A long narrow passage. The floor tiles look suspiciously uniform. Scratch marks on the walls suggest others have met their end here.",
    region_id = crypt_region.id,
    lighting = "dark",
    exits = {south = hall.id},
    trapped = true
})

-- Room 5: Treasury
local treasury = game.create_object("/rooms/crypt-treasury", "room", nil, {
    name = "Crypt Treasury",
    description = "A chamber filled with dusty coffers and ancient artifacts. Gold coins are scattered across the floor, but something feels wrong.",
    region_id = crypt_region.id,
    lighting = "dim",
    exits = {south = trap_corridor.id}
})

-- Room 6: Boss Chamber
local boss_chamber = game.create_object("/rooms/throne-of-crypt-lord", "room", nil, {
    name = "Throne of the Crypt Lord",
    description = "A vast chamber with a throne of blackened bone at its center. Dark energy crackles in the air, and ancient runes glow with sickly green light.",
    region_id = crypt_region.id,
    lighting = "magical_darkness",
    exits = {west = hall.id}
})

-- Update entrance exits
game.update_object(entrance.id, {
    exits = {south = antechamber.id}
})

-- Update antechamber exits
game.update_object(antechamber.id, {
    exits = {north = entrance.id, west = hall.id}
})

-- Update hall exits
game.update_object(hall.id, {
    exits = {east = antechamber.id, north = trap_corridor.id, south = boss_chamber.id}
})
```

### Healing Potion Code

```lua
local potion_code = [[
return {
    on_use = function(args)
        local potion = game.get_object(args.object_id)

        -- Check if already consumed
        if potion.metadata.consumed then
            game.send(args.actor_id, "The vial is empty.")
            return false
        end

        -- Roll healing
        local heal_dice = potion.metadata.heal_dice or "2d4+2"
        local healing = game.roll_dice(heal_dice)

        -- Apply healing
        local player = game.get_object(args.actor_id)
        local current_hp = player.metadata.hp or 0
        local max_hp = player.metadata.max_hp or 100
        local new_hp = math.min(current_hp + healing, max_hp)

        game.update_object(args.actor_id, {hp = new_hp})
        game.send(args.actor_id,
            string.format("You drink the healing potion and recover %d HP! (HP: %d/%d)",
                healing, new_hp, max_hp))

        -- Destroy the potion
        game.delete_object(args.object_id)
        return true
    end
}
]]

local potion_hash = game.store_code(potion_code)

-- Create potions (path, class, parent_id, props)
local potion1 = game.create_object("/items/minor-healing-potion", "healing_potion", antechamber.id, {
    name = "Minor Healing Potion",
    description = "A small vial of red liquid that shimmers with restorative magic.",
    heal_dice = "2d4+2",
    weight = 0,
    value = 50,
    code_hash = potion_hash
})
```

### Locked Chest Code

```lua
local chest_code = [[
return {
    on_use = function(args)
        local chest = game.get_object(args.object_id)

        if chest.metadata.locked then
            -- Check if player has the key
            local inventory = game.all_inventory(args.actor_id)
            local has_key = false

            for _, item in ipairs(inventory) do
                if item.id == chest.metadata.key_id then
                    has_key = true
                    break
                end
            end

            if has_key then
                game.update_object(args.object_id, {locked = false})
                game.send(args.actor_id, "You unlock the chest with the rusty key!")
                game.broadcast(game.environment(args.object_id).id,
                    "Click! The chest unlocks.")
            else
                game.send(args.actor_id, "The chest is locked. You need a key.")
                return false
            end
        end

        -- Open chest and spawn loot if not already spawned
        if not chest.metadata.loot_spawned then
            game.update_object(args.object_id, {loot_spawned = true})

            -- Spawn random treasure
            local gold = game.roll_dice("3d6") * 10
            game.create_object("/items/gold-coins-" .. gold, "item", args.object_id, {
                name = gold .. " Gold Coins",
                description = "A pile of ancient gold coins.",
                value = gold,
                weight = 1
            })

            game.send(args.actor_id,
                "You open the chest and find " .. gold .. " gold coins!")
        else
            game.send(args.actor_id, "The chest is already empty.")
        end

        return true
    end
}
]]

local chest_hash = game.store_code(chest_code)

-- Create the key first
local crypt_key = game.create_object("/items/rusty-crypt-key", "item", entrance.id, {
    name = "Rusty Crypt Key",
    description = "An ancient iron key, covered in rust but still functional.",
    weight = 0,
    value = 0
})

-- Create locked chest
local treasure_chest = game.create_object("/items/ancient-treasure-chest", "locked_chest", treasury.id, {
    name = "Ancient Treasure Chest",
    description = "A heavy iron chest covered in dust. It appears to be locked.",
    locked = true,
    key_id = crypt_key.id,
    capacity = 20,
    weight = 50,
    fixed = true,
    code_hash = chest_hash
})
```

### Trap Code

```lua
local trap_code = [[
return {
    on_enter = function(args)
        local room = game.get_object(args.object_id)

        if not room.metadata.trapped then
            return true
        end

        -- 50% chance to trigger trap
        if game.roll_dice("1d100") <= 50 then
            game.send(args.actor_id, "You hear a click as you step on a pressure plate!")

            -- Delayed damage
            game.call_out(1.0, "spring_trap", args.actor_id)
        end

        return true
    end,

    spring_trap = function(args)
        -- args is the victim's ID passed from on_enter
        local victim_id = args
        local victim = game.get_object(victim_id)

        if not victim then return end

        local damage = game.roll_dice("2d6")
        local new_hp = (victim.metadata.hp or 100) - damage

        game.update_object(victim_id, {hp = new_hp})
        game.send(victim_id,
            string.format("Poison darts shoot from the walls! You take %d damage! (HP: %d)",
                damage, new_hp))

        -- Check for death
        if new_hp <= 0 then
            game.send(victim_id, "The poison finishes you off...")
        end
    end
}
]]

local trap_hash = game.store_code(trap_code)

-- Apply trap code to the corridor
game.update_object(trap_corridor.id, {
    code_hash = trap_hash
})
```

### Skeleton Warrior Code

```lua
local skeleton_code = [[
return {
    on_init = function(args)
        -- Start combat heartbeat
        game.set_heart_beat(3000)  -- Every 3 seconds
    end,

    heart_beat = function(args)
        local skeleton = game.get_object(args.object_id)
        if not skeleton then return end

        local room = game.environment(args.object_id)
        if not room then return end

        -- Find living players in the room
        local targets = game.get_living_in(room.id)
        local player_target = nil

        for _, entity in ipairs(targets) do
            if entity.class == "player" and entity.id ~= args.object_id then
                player_target = entity
                break
            end
        end

        if player_target and skeleton.metadata.aggro then
            -- Attack!
            local attack_roll = game.roll_dice("1d20") + (skeleton.metadata.attack_bonus or 2)
            local target_ac = player_target.metadata.armor_class or 10

            if attack_roll >= target_ac then
                local damage = game.roll_dice("1d6+2")
                local new_hp = (player_target.metadata.hp or 100) - damage

                game.update_object(player_target.id, {hp = new_hp})
                game.send(player_target.id,
                    string.format("%s slashes at you with a rusty sword for %d damage! (HP: %d)",
                        skeleton.name, damage, new_hp))
                game.broadcast(room.id,
                    skeleton.name .. " attacks " .. (player_target.name or "someone") .. "!")
            else
                game.send(player_target.id, skeleton.name .. " swings and misses!")
            end
        end
    end,

    on_damage = function(args)
        local skeleton = game.get_object(args.object_id)

        -- Check for radiant vulnerability (double damage)
        if args.damage_type == "radiant" and skeleton.metadata.radiant_vulnerable then
            args.damage = args.damage * 2
            game.broadcast(game.environment(args.object_id).id,
                "The radiant energy sears the undead bones!")
        end

        -- Check for necrotic immunity
        if args.damage_type == "necrotic" and skeleton.metadata.necrotic_immune then
            args.damage = 0
            game.broadcast(game.environment(args.object_id).id,
                "The necrotic energy has no effect on the skeleton!")
        end

        return args.damage
    end,

    on_death = function(args)
        local skeleton = game.get_object(args.object_id)
        local room = game.environment(args.object_id)

        game.broadcast(room.id,
            skeleton.name .. " collapses into a pile of bones!")

        -- Schedule respawn
        local respawn_time = skeleton.metadata.respawn_time
        if respawn_time then
            game.call_out(respawn_time / 1000, "respawn", nil)
        else
            game.delete_object(args.object_id)
        end
    end,

    respawn = function(args)
        local skeleton = game.get_object(args.object_id)
        if not skeleton then return end

        -- Reset HP
        game.update_object(args.object_id, {
            hp = skeleton.metadata.max_hp or 25
        })

        local room = game.environment(args.object_id)
        if room then
            game.broadcast(room.id,
                "Bones rattle and reassemble... " .. skeleton.name .. " rises again!")
        end
    end
}
]]

local skeleton_hash = game.store_code(skeleton_code)

-- Create skeleton guards (path, class, parent_id, props)
local skeleton1 = game.create_object("/npcs/skeleton-guard", "skeleton_warrior", antechamber.id, {
    name = "Skeleton Guard",
    description = "An animated skeleton in rusted armor, eye sockets glowing with unholy light.",
    hp = 25,
    max_hp = 25,
    attack_bonus = 3,
    armor_class = 13,
    aggro = true,
    respawn_time = 180000,  -- 3 minutes
    code_hash = skeleton_hash
})

local skeleton2 = game.create_object("/npcs/skeleton-warrior", "skeleton_warrior", hall.id, {
    name = "Skeleton Warrior",
    description = "A larger skeleton wielding a notched greatsword.",
    hp = 35,
    max_hp = 35,
    attack_bonus = 4,
    armor_class = 14,
    aggro = true,
    respawn_time = 180000,
    code_hash = skeleton_hash
})
```

### Crypt Lord Boss Code

```lua
local boss_code = [[
return {
    on_init = function(args)
        game.set_heart_beat(4000)  -- Every 4 seconds

        -- Initialize boss state
        local boss = game.get_object(args.object_id)
        game.update_object(args.object_id, {
            phase = 1,
            enraged = false
        })
    end,

    heart_beat = function(args)
        local boss = game.get_object(args.object_id)
        if not boss then return end

        local room = game.environment(args.object_id)
        if not room then return end

        -- Check if dead
        if (boss.metadata.hp or 0) <= 0 then
            return
        end

        -- Find player target
        local targets = game.get_living_in(room.id)
        local player_target = nil

        for _, entity in ipairs(targets) do
            if entity.class == "player" then
                player_target = entity
                break
            end
        end

        if not player_target then return end

        -- Phase transitions
        local hp_percent = (boss.metadata.hp or 0) / (boss.metadata.max_hp or 200) * 100

        if hp_percent <= 50 and boss.metadata.phase == 1 then
            game.update_object(args.object_id, {phase = 2})
            game.broadcast(room.id,
                "The Crypt Lord roars in fury! Dark energy swirls around him!")
            game.broadcast(room.id,
                "\"YOU DARE CHALLENGE ME IN MY OWN DOMAIN?!\"")
        end

        if hp_percent <= 25 and not boss.metadata.enraged then
            game.update_object(args.object_id, {enraged = true})
            game.broadcast(room.id,
                "The Crypt Lord enters a berserk rage! His attacks become frenzied!")
        end

        -- Combat based on phase
        local phase = boss.metadata.phase or 1

        if phase == 1 then
            -- Normal attack
            self_attack(args.object_id, player_target, "1d10+5", "physical")
        else
            -- Phase 2: multi-attack
            self_attack(args.object_id, player_target, "1d10+5", "physical")

            -- 50% chance for necrotic bolt
            if game.roll_dice("1d100") <= 50 then
                game.send(player_target.id,
                    "The Crypt Lord hurls a bolt of necrotic energy at you!")
                local necrotic_damage = game.roll_dice("2d6")
                local new_hp = (player_target.metadata.hp or 0) - necrotic_damage
                game.update_object(player_target.id, {hp = new_hp})
                game.send(player_target.id,
                    string.format("The necrotic bolt deals %d damage! (HP: %d)",
                        necrotic_damage, new_hp))
            end
        end

        -- Enrage bonus
        if boss.metadata.enraged then
            -- Extra attack when enraged
            self_attack(args.object_id, player_target, "1d8+3", "physical")
        end
    end,

    on_death = function(args)
        local boss = game.get_object(args.object_id)
        local room = game.environment(args.object_id)

        game.broadcast(room.id, "")
        game.broadcast(room.id, "The Crypt Lord lets out a terrible shriek!")
        game.broadcast(room.id, "\"NO! THIS CANNOT BE! I AM ETERNAL!\"")
        game.broadcast(room.id, "")
        game.broadcast(room.id, "His form dissolves into shadows, leaving behind his crown...")

        -- Drop epic loot (path, class, parent_id, props)
        game.create_object("/items/soulreaver", "cursed_weapon", room.id, {
            name = "Soulreaver",
            description = "The Crypt Lord's blade, still pulsing with dark energy.",
            damage_dice = "2d6",
            damage_type = "necrotic",
            curse_name = "Soul Drain",
            bonus_damage_dice = "1d6",
            bonus_damage_type = "necrotic"
        })

        game.create_object("/items/crown-of-crypt-lord", "item", room.id, {
            name = "Crown of the Crypt Lord",
            description = "A crown of black iron, set with a single blood-red gem.",
            value = 5000,
            weight = 2
        })

        -- Don't respawn - permanent kill
        game.delete_object(args.object_id)
    end
}

-- Helper function for attacks (defined inside the code block)
function self_attack(attacker_id, target, damage_dice, damage_type)
    local attacker = game.get_object(attacker_id)
    local attack_roll = game.roll_dice("1d20") + (attacker.metadata.attack_bonus or 5)
    local target_ac = target.metadata.armor_class or 10

    if attack_roll >= target_ac then
        local damage = game.roll_dice(damage_dice)
        local new_hp = (target.metadata.hp or 0) - damage
        game.update_object(target.id, {hp = new_hp})
        game.send(target.id,
            string.format("%s strikes you for %d %s damage! (HP: %d)",
                attacker.name, damage, damage_type, new_hp))
    else
        game.send(target.id, attacker.name .. " swings but misses!")
    end
end
]]

local boss_hash = game.store_code(boss_code)

-- Create the boss (path, class, parent_id, props)
local crypt_lord = game.create_object("/npcs/valdris-crypt-lord", "npc", boss_chamber.id, {
    name = "Valdris the Crypt Lord",
    description = "A towering skeletal figure in ornate black armor. A crown of twisted iron sits upon his skull, and his eye sockets burn with baleful green fire. He grips a massive sword that drips with shadow.",
    hp = 200,
    max_hp = 200,
    attack_bonus = 7,
    armor_class = 16,
    aggro = true,
    phase = 1,
    enraged = false,
    code_hash = boss_hash
})
```

### Cursed Weapon Loot

```lua
local cursed_weapon_code = [[
return {
    on_use = function(args)
        local weapon = game.get_object(args.object_id)

        -- Equip message
        game.send(args.actor_id,
            "You grasp " .. weapon.name .. ". Dark whispers fill your mind...")
        game.send(args.actor_id,
            "Curse: " .. (weapon.metadata.curse_name or "Unknown"))

        -- Bonus damage notification
        game.send(args.actor_id,
            string.format("This weapon deals %s %s damage on each hit.",
                weapon.metadata.bonus_damage_dice or "0",
                weapon.metadata.bonus_damage_type or "dark"))

        return true
    end
}
]]

local cursed_hash = game.store_code(cursed_weapon_code)

-- Flamebrand in treasury as bonus loot
game.create_object("/items/flamebrand", "cursed_weapon", treasury.id, {
    name = "Flamebrand",
    description = "A longsword with flames dancing along its blade. The hilt is warm to the touch.",
    damage_dice = "1d8",
    damage_type = "physical",
    curse_name = "Burning Grip",
    bonus_damage_dice = "1d6",
    bonus_damage_type = "fire",
    value = 1500,
    code_hash = cursed_hash
})
```

### Complete Universe Setup Script

To create the entire Cursed Crypt, run all the above code blocks in order:

1. Define custom classes
2. Create region
3. Create rooms and link exits
4. Store all handler code
5. Create items, NPCs, and traps
6. Create the boss

The dungeon is now ready for players to explore!

---

## Summary

You've learned:
- Creating rooms, items, and NPCs
- Defining custom classes with inheritance
- Writing handler code for behaviors
- Using timers for delayed and recurring events
- Messaging players and broadcasting events
- Managing permissions

For the complete API reference, see `langref.md`.
