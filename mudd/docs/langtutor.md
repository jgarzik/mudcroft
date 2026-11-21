# Mudcroft Content Creator Tutorial

This is a hands-on guide to building a Mudcroft world in Lua: rooms,
items, NPCs, custom verbs, hooks and timers. It ends with a complete
mini-dungeon, *The Cursed Crypt*.

Look up exact signatures in the [Lua API reference](langref.md). Access
levels and path grants are covered in [permissions](permissions.md).

---

## Before you start

### Who can do what

- **Wizards and above** write Lua. They run it with the `eval` command and store object code with `game.store_code`.
- **Builders** can't run Lua. They create objects with the `create` command inside the paths they've been granted, and they own what they create.
- **Admins and universe owners** can also run Lua script files with `run_script`.

The level order is `player < builder < wizard < admin < owner`; details
are in [permissions](permissions.md).

### Running Lua

There are three ways to run Lua as a wizard:

1. **`eval <lua>`** in the game client runs one line of Lua as you.
   - It shows the chunk's **return value**: `eval return 1 + 2` shows `3`, and `eval 1 + 2` works too.
   - `print` does nothing in Mudcroft, so use `return` (or `game.send`) to see results.
   - Tables display as `[table]`, so return a string built from the fields you want.
   - The client sends one line at a time. The examples below are spread over several lines for readability. To paste one into `eval`, join the lines and **delete any `--` comments first**, because a comment would swallow the rest of the line.
2. **A universe library function.** Libraries are Lua files loaded into every script in your universe. They are installed when the universe is created; see [API](API.md) and [admin](admin.md). Define a function such as `build_crypt()` in a library, then run `eval return build_crypt()`. Part B uses this approach.
3. **`run_script`**: `POST /universe/{id}/run_script` runs a file from the server's `scripts/` directory. It needs admin or universe ownership; see [API](API.md). `scripts/cave_adventure.lua` is an example.

Both `eval` and `run_script` allow up to 10,000 database calls and 10
million instructions per run, which is enough to build a whole area at
once.

### Built-in commands

| Command | Who | What it does |
|---|---|---|
| `look` / `l` | everyone | show the room |
| `north` `south` `east` `west` `up` `down` (or `n` `s` `e` `w` `u` `d`) | everyone | walk through an exit |
| `say <text>` | everyone | others see `<name> says: <text>`; you see `You say: <text>` |
| `get` / `take <item>`, `drop <item>` | everyone | pick up or drop items (anything not `fixed` and not alive) |
| `inventory` / `inv` / `i` | everyone | list what you carry |
| `attack` / `kill <npc>` | everyone | the built-in combat round (see Lesson 3) |
| `help` | everyone | list commands for your level |
| `create <class> <path> "<name>" ["<desc>"] [key=value ...]` | builder+ | create an object (`parent=<id>` sets its container) |
| `goto <room_id>` | wizard+ | teleport to a room |
| `setportal [room_id]` | wizard+ | set the spawn room for new arrivals |
| `eval <lua>` | wizard+ | run Lua |

Any other command is checked against the custom verbs that objects
register (Lesson 5). If none matches, the player sees `Unknown command`.

### Setting the spawn portal

Players arrive in the universe's **portal room**. Until a portal is set,
they see `Universe not initialized. Wizards: use 'setportal' command.`

1. Build at least one room (Lesson 1).
2. Go there with `goto /d/town/square`.
3. Run `setportal`, or name the room directly: `setportal /d/town/square`.
4. You'll see `Portal set to: Town Square (/d/town/square)`.

You choose every object's path yourself, so you always know the room IDs.
If your rooms are placed inside a region object (Lesson 1), you can list
them:

```lua
eval local t = {} for _, r in ipairs(game.get_children("/d/town", {class = "room"})) do t[#t+1] = r.id end return table.concat(t, ", ")
```

Where players appear:

- **New players** appear in the portal room.
- **Returning players** reappear in the last *safe* room they stood in. A safe room is the portal room at that time, a room with `is_portal = true`, or the player's `workroom_id`. If that room no longer exists, they go to the portal.

The portal is set **only** by `setportal`. Universe config keys such as
`spawn_room` are not read by the server.

---

## Part A: Lessons

All examples are run by a wizard. Objects you create are **owned by
you**, and any code attached to them runs **as you** (see Lesson 5).

### Lesson 1: Rooms

A room is an object of class `room`. Its `exits` property maps
directions to destination room IDs.

```lua
local area = game.create_object("/d/town", "region", nil, {
    name = "Town District",
    environment_type = "urban",
})

local square = game.create_object("/d/town/square", "room", "/d/town", {
    name = "Town Square",
    description = "Cobblestones worn smooth by centuries of feet. A fountain burbles in the center.",
    exits = {north = "/d/town/tavern"},
})

local tavern = game.create_object("/d/town/tavern", "room", "/d/town", {
    name = "The Rusty Tankard",
    description = "A warm tavern that smells of ale and roasting meat.",
    exits = {south = "/d/town/square"},
})

return square.error or tavern.error or "built " .. square.id .. " and " .. tavern.id
```

Key points:

- **Paths.** The first argument is the object's path. Paths are lowercase segments of letters, digits and hyphens, and each segment starts with a letter. Paths are unique across the whole server.
- **Grouping by prefix.** Keep an area under one prefix such as `/d/town/...`. A single path grant then covers it, and `game.broadcast_region("/d/town", msg)` reaches every room in it.
- **Parent.** The third argument is the container. Putting rooms inside the region object is optional; it lets you list them with `get_children`.
- **Exits.** An exit is just a room ID string, so it can point at a room you haven't created yet. The walking directions are `north`, `south`, `east`, `west`, `up` and `down`.
- **Errors.** `create_object` returns `{error = "..."}` instead of an object if the path is invalid or you lack permission. Check `.error`.
- **Updating.** `game.update_object(id, {exits = {...}})` replaces the whole `exits` table, so include every exit.

### Lesson 2: Items

```lua
local torch = game.create_object("/d/town/square/torch", "item", "/d/town/square", {
    name = "wooden torch",
    description = "A torch wrapped in oil-soaked rags.",
    weight = 1,
    value = 5,
})

local fountain = game.create_object("/d/town/square/fountain", "item", "/d/town/square", {
    name = "stone fountain",
    description = "An ornate fountain carved as a dragon.",
    fixed = true,
})
return torch.id .. ", " .. fountain.id
```

Which properties matter:

- `name` is what players type: `take torch` matches any object in the room whose name contains "torch".
- `fixed = true` stops players taking or moving the object.
- `weight` and `value` are data for your own scripts. The server doesn't read them.

`game.create_object` fills in the class defaults (`weight = 0`,
`value = 0`, `fixed = false` for `item`), then applies your values on
top. The builder `create` command doesn't apply class defaults.

### Lesson 3: NPCs and the built-in combat

```lua
local goblin = game.create_object("/d/town/tavern/goblin", "npc", "/d/town/tavern", {
    name = "goblin scout",
    description = "A small green creature with sharp teeth.",
    hp = 15,
    max_hp = 15,
    fixed = true,
})
return goblin.error or goblin.id
```

This is what the built-in `attack goblin` command does:

- It can target objects of class `npc`, of a class derived from `npc` (Lesson 4), or of class `monster`.
- On the first attack, the combat system copies `hp` and `max_hp` from the NPC. From then on it tracks HP itself.
- Each attack rolls a d20 against armor class 10. A hit deals 1d6 damage, doubled on a natural 20. The NPC's `armor_class` and `attack_bonus` properties are **not** used.
- A hit runs the NPC's `on_damage` hook. A kill runs `on_death` and removes the NPC from the room. The NPC object isn't deleted.
- **NPCs don't fight back on their own.** Part B shows how to add that in Lua.

Players can't `take` NPCs or other living things. `fixed = true` still
stops non-builders moving them with `game.move_object`. `aggro` and
`respawn_time` exist as class properties but do nothing; respawning is
done in Lua (Part B).

### Lesson 4: Custom classes

A class supplies default properties. A class you define belongs to
your universe (the built-in classes exist everywhere) and is stored, so it
survives a restart. Only wizards+ can define them, built-in classes can't
be redefined, and a custom class can be redefined only by the account that
defined it (or an admin).

```lua
game.define_class("potion", {
    parent = "item",
    properties = {
        heal_dice = {type = "string", default = "2d4+2"},
    },
})

local p = game.create_object("/d/town/tavern/potion", "potion", "/d/town/tavern", {
    name = "red potion",
    description = "It shimmers with healing energy.",
})
return p.metadata.heal_dice   -- "2d4+2", inherited from the class
```

Things to know:

- **Restarts.** Custom classes are currently kept **in memory only** and are lost when the server restarts. Objects keep the properties they were created with, so you only need to define the class again before creating more objects from it.
- **Inheritance.** A new object gets the defaults of its class and every ancestor (`potion` → `item` → `thing`), and your props override them. Empty-string defaults and a class's `code_hash` default are not copied; attach code by passing `code_hash` in the props.
- **Reading properties.** Custom properties appear under `obj.metadata`. `name` and `description` are also copied to `obj.name` and `obj.description`.
- **Checking class.** `game.is_a(id, "item")` checks inheritance, and so do the `attack` and `take` commands. `game.get_living_in` and the `get_children` class filter match class names exactly, so a monster of a custom `npc` subclass is missing from `get_living_in`; use `all_inventory` with `game.is_a` to find it.

### Lesson 5: Object code, hooks and verbs

Behaviour lives in **object code**: Lua source that returns a table of
handler functions. You store it with `game.store_code` (wizard+), then
attach it by setting `code_hash`.

#### A room that greets visitors

```lua
local h = game.store_code([[
return {
    on_enter = function(a)
        if a.from == nil then return end   -- nil when the player just logged in
        game.send(a.actor_id, "The smell of fresh bread drifts over from the bakery.")
        game.broadcast(a.room_id, "Someone wanders into the square.", a.actor_id)
    end,
}
]])
return game.update_object("/d/town/square", {code_hash = h})
```

#### Who the code runs as

Object code always runs as the **object's owner**, the account in
`owner_id`, and never as the player who triggered it. This follows LPC.

- The triggering player is passed as data, in `args.actor_id`.
- What the code may change depends on the owner. Your wizard-owned room can update any object. A builder's object can change only that builder's own objects and paths.
- An object with no owner runs with no permissions at all.

Two rules follow from this:

- **Attaching code.** Only the object's owner, or someone who outranks the owner, may set `code_hash` with `update_object`. You can also set `code_hash` in the props of `create_object`. An unknown hash is rejected, and `code_hash = nil` detaches the code.
- **Player IDs.** `args.actor_id` is a player **object** ID such as `/players/p-1234...`. To get the player's account ID, use `game.get_object(a.actor_id).owner_id`.

#### Which hooks fire

| Hook | Fires when | Runs on | Extra `args` |
|---|---|---|---|
| `on_enter` | a player walks in, `goto`s in, or logs in and spawns there | room | `from` (nil on spawn), `direction` (walking only) |
| `on_leave` | a player walks out or `goto`s away | room | `to`, `direction` (walking only) |
| `on_damage` | the `attack` command hits | NPC | `attacker_id`, `damage` |
| `on_death` | the `attack` command kills | NPC | `killer_id` |
| `heart_beat` | the object's heartbeat is due (Lesson 6) | object | no `actor_id` |

The first four hooks also get `object_id`, `actor_id` and `room_id`.
`heart_beat` gets `object_id`, `room_id` and `args` (always `nil`).

These handler names **never fire on their own**: `on_create`,
`on_destroy`, `on_init`, `on_move`, `on_use`, `ai_idle_tick` and
`ai_combat_tick`. Engine hooks (the table above, plus `on_create` and
`on_destroy`) can't be triggered with `game.use_object` either. Other
limits:

- `game.move_object` fires no hooks.
- Region objects get no hooks.
- Hook return values are ignored.
- By the time `on_death` runs, the NPC is already out of the room, so use `args.room_id`, not `game.environment(...)`.

#### Custom verbs

Most interaction comes from verbs that objects register with
`game.add_action(verb, object_id, method)`. When a player types
`pull lever`, the server finds the lever's `pull` registration. It then
runs the `pull` handler as the lever's owner with
`{verb, args, actor_id, object_id, room_id}`. **The handler's return
value is the player's reply.**

```lua
local h = game.store_code([[
return {
    pull = function(a)
        local lever = game.get_object(a.object_id)
        local up = not lever.metadata.down
        game.update_object(a.object_id, {down = up})
        game.broadcast(a.room_id, "Somewhere, stone grinds on stone.", a.actor_id)
        if up then return "You pull the lever down. Clunk!" end
        return "You push the lever back up."
    end,
}
]])
game.create_object("/d/town/square/lever", "item", "/d/town/square", {name = "lever", fixed = true, code_hash = h})
return game.add_action("pull", "/d/town/square/lever", "pull")
```

How verbs behave:

- **Scope.** `eval` has no room context, so it registers an **object action**. The verb works wherever the object is: in the room, or in a player's inventory.
- **Room actions.** Calling `add_action` inside a hook or verb handler registers a **room action** for that room instead. It only works while the object is nearby.
- **Permission.** To add or remove an object's actions you need Modify permission on it **and** must own it or be a wizard+ who outranks its owner, because the handler runs as the owner. Ownerless objects need only Modify.
- **Private handlers.** `add_action` refuses methods named `_something` and engine hooks such as `on_enter` or `heart_beat`; keep helpers private by starting their names with `_`.
- **Built-in commands win.** You can't override `look`, `get`, `say` and the other built-ins.
- **Choosing an object.** If several nearby objects register the same verb, the one whose name matches what the player typed is chosen.
- **Replies.** Return `nil` to send no reply; that's useful when the handler already used `game.send`. If the handler raises an error, players see `Something went wrong.` and wizards see the real error.
- **Restarts.** Actions are kept **in memory only** and are lost when the server restarts. Keep the `add_action` calls in a function you can run again (Part B's `crypt_verbs()`), or register them from a room's `on_enter` hook.

#### Calling another object's code

`game.use_object(obj_id, actor_id, verb, target_id)` runs a handler on
another object.

- `"use"` maps to `on_use`, `"look"` to `on_look`, `"hit"` to `on_hit` and `"init"` to `on_init`. Any other verb names the handler directly.
- The handler runs in its own sandbox **as that object's owner** and gets `{object_id, actor_id, verb, target_id, code_hash}`.
- It returns plain data: tables, strings, numbers and booleans.
- Handlers whose names start with `_` are private, and engine hooks (`on_enter`, `on_leave`, `on_damage`, `on_death`, `heart_beat`, `on_create`, `on_destroy`) can't be called this way; such calls return `nil`.
- `actor_id` is whatever the caller passed, so don't trust it as proof of who acted.
- One top-level execution may make at most 32 `use_object` calls, across all nesting levels (at most 8 deep).

A room uses this in Part B to wake its monsters.

### Lesson 6: Timers

Timers belong to an object, so `game.call_out` and `game.set_heart_beat`
only work **inside that object's code**. In `eval`, `call_out` returns
`""` and `set_heart_beat` does nothing.

| Function | What it does |
|---|---|
| `game.call_out(delay_secs, method, args)` | Run `method` on this object once, after `delay_secs` (fractions allowed). `args` is an optional **string**. Returns a timer ID, or `""`. Up to 32 pending per object. Saved across restarts. |
| `game.remove_call_out(timer_id)` | Cancel a pending call_out. |
| `game.set_heart_beat(interval_ms)` | Run this object's `heart_beat` handler every `interval_ms` (at least 100). Memory only: stops on restart. |
| `game.remove_heart_beat()` | Stop this object's heartbeat. |

Timer callbacks run as the object's owner and receive
`{object_id, room_id, args}`.

- `room_id` is the object's `parent_id` at the time the timer fires, so for a room's own timer it is the room's container, not the room.
- There is no `actor_id`. Pass the player's ID in `args` if you need it.
- Deleting the object cancels its timers.

A bell that chimes and a clock that ticks, driven by verbs:

```lua
local h = game.store_code([[
return {
    ring = function(a)
        game.call_out(2, "chime", a.actor_id)   -- remember who rang it
        return "You pull the bell rope."
    end,
    chime = function(a)
        game.send(a.args, "The bell answers you with a deep BONG.")
        game.broadcast(a.room_id, "A bell tolls.", a.args)
    end,
    wind = function(a)
        game.set_heart_beat(5000)
        return "You wind the clock."
    end,
    heart_beat = function(a)
        local clock = game.get_object(a.object_id)
        local ticks = (clock.metadata.ticks or 0) + 1
        game.update_object(a.object_id, {ticks = ticks})
        game.broadcast(a.room_id, "The clock ticks.")
        if ticks >= 6 then
            game.update_object(a.object_id, {ticks = 0})
            game.remove_heart_beat()
        end
    end,
}
]])
game.create_object("/d/town/square/bell", "item", "/d/town/square", {name = "bell clock", fixed = true, code_hash = h})
game.add_action("ring", "/d/town/square/bell", "ring")
game.add_action("wind", "/d/town/square/bell", "wind")
return "ok"
```

The handler code is re-run fresh for every call, so Lua variables don't
survive between calls. Keep state in properties, as `ticks` does here.

### Lesson 7: Messaging

| Function | Reaches |
|---|---|
| `game.send(player_id, msg)` | one player |
| `game.broadcast(room_id, msg, exclude_id)` | everyone in the room except `exclude_id` (optional) |
| `game.broadcast_region(region_id, msg)` | everyone in a room whose path is under `region_id`, or whose parent is `region_id` |

```lua
-- inside a verb handler
game.broadcast(a.room_id, "The torches flicker ominously.", a.actor_id)
game.broadcast_region("/d/town", "The town bell rings out across the district.")
return "You blow out the nearest torch."
```

How delivery works:

- **Timing.** Messages are queued and delivered **after the script finishes**, in order. A script that errors still delivers what it queued before the error.
- **Reply vs. message.** The usual pattern is to return the actor's message as the verb's reply and broadcast to the others with `exclude_id`. The built-in `take`, `drop` and `say` work this way.
- **Leaving and arriving.** In `on_leave`, the player is still in the room, so exclude them from a broadcast meant for those staying behind. In `on_enter`, the player has already arrived.
- **Who receives.** Only connected players get messages. Player IDs are player object IDs, as in `args.actor_id`.

### Lesson 8: Permissions

Here's how a wizard sets up a builder for an area. Changing an access
level needs **admin+**; granting paths needs wizard+.

```lua
-- as an admin: promote the account
game.set_access_level("<account-id>", "builder")

-- as a wizard: grant the area; true lets them sub-delegate
local g = game.grant_path("<account-id>", "/d/town", true)
return g.error or ("granted " .. g.path_prefix .. " as " .. g.id)
```

Inspect and revoke grants:

```lua
local out = {}
for _, g in ipairs(game.get_path_grants("<account-id>")) do
    out[#out + 1] = g.path_prefix .. " (delegate=" .. tostring(g.can_delegate) .. ", id=" .. g.id .. ")"
end
return table.concat(out, "; ")
```

```lua
return game.revoke_path("<grant-id>")   -- true, false if not found, or {error=...}
```

To test something as a lower-level account, use `game.set_actor`. Only
wizards+ can call it, and only for accounts **below** their own level.
It returns `false` if refused.

```lua
game.set_actor("<builder-account-id>")
local r = game.create_object("/d/elsewhere/rock", "item", nil, {name = "rock"})
game.set_actor(nil)
return r.error or "allowed"   -- denied: no grant on /d/elsewhere
```

You can also ask before acting:

```lua
local obj = game.get_object("/d/town/square/lever")
local r = game.check_permission("modify", obj.id, obj.metadata.fixed, obj.owner_id)
return r.allowed and "yes" or ("no: " .. r.error)
```

How failures show up:

- A denied `create_object` or `clone_object` returns `{error = ...}`.
- A denied `update_object`, `delete_object` or `move_object` raises an error. Wrap it in `pcall` if you want to handle that.

The rules in brief:

- Wizards+ may change anything.
- Owners may change their own objects.
- Builders may change objects under their path grants.
- Anyone may move objects that aren't `fixed`.

Full details: [permissions](permissions.md).

---

## Part B: The Cursed Crypt

A complete six-room dungeon. It has:

- a key and a locked chest
- healing potions with a `drink` verb
- a trapped corridor that uses a timer
- skeletons that wake when you enter, fight back, and respawn
- a boss with a second phase

Everything here uses features that work today.

### How it's organised

The crypt is a **universe library**, `crypt.lua`. Add it when the
universe is created (the `libs` field of `/universe/create`, or a `.lua`
file in the ZIP upload; see [API](API.md)).

A library runs before *every* script in the universe, so it only
**defines** things:

- `build_crypt()` builds the area once.
- `crypt_verbs()` registers the custom verbs. Run it after building and again after every server restart, because verbs aren't saved.
- `crypt_strike()` is a shared attack helper. Object code can call it because libraries are loaded into object code too.

To play it, connect as a wizard and run:

```
eval return build_crypt()
eval return crypt_verbs()
setportal /d/crypt/entrance
```

Everything is created and owned by the wizard who runs `build_crypt()`,
so all the crypt's code runs with that wizard's rights. That's needed
here, because the code updates the HP of player objects.

Each player has a separate player object in every universe they visit,
created on their first visit, so the crypt hurts, heals and hands items to
the visitor's player object in this universe only.

`build_crypt()` is meant for a fresh universe. To rebuild, delete the
`/d/crypt/...` objects first, contents before containers.

The monsters' code calls `crypt_strike`, so `crypt.lua` must be
installed as a library. Running it once with `run_script` is not enough.

### `crypt.lua`

```lua
-- crypt.lua: universe library for "The Cursed Crypt".
-- Libraries run before every script, so this file only defines things.
--   eval return build_crypt()   -- once, on a fresh universe
--   eval return crypt_verbs()   -- after building, and after every restart

local AREA = "/d/crypt"
local Code = {}

-- Shared attack helper, also used by monster code.
-- Player HP lives in the player object's hp property.
function crypt_strike(me, target_id, dice)
    local target = game.get_object(target_id)
    if not target or target.parent_id ~= me.parent_id then return end
    local roll = game.roll_dice("1d20") + (me.metadata.attack_bonus or 0)
    if roll < (target.metadata.armor_class or 10) then
        game.send(target_id, me.name .. " swings at you and misses.")
        return
    end
    local dmg = game.roll_dice(dice or "1d6")
    local hp = (target.metadata.hp or 100) - dmg
    game.send(target_id, string.format("%s hits you for %d damage! (HP: %d)", me.name, dmg, math.max(hp, 0)))
    game.broadcast(me.parent_id, me.name .. " hits " .. (target.name or "someone") .. ".", target_id)
    if hp <= 0 then
        hp = target.metadata.max_hp or 100
        game.send(target_id, "You black out... and wake up, battered but whole.")
    end
    game.update_object(target_id, {hp = hp})
end

-- Healing potion: "drink potion" (must be carried)
Code.potion = [=[
return {
    drink = function(a)
        local potion = game.get_object(a.object_id)
        local me = game.get_object(a.actor_id)
        if not me then return "Only adventurers can drink potions." end
        if potion.parent_id ~= a.actor_id then return "Pick it up first." end
        local heal = game.roll_dice(potion.metadata.heal_dice)
        local max_hp = me.metadata.max_hp or 100
        local hp = math.min((me.metadata.hp or max_hp) + heal, max_hp)
        game.update_object(a.actor_id, {hp = hp})
        game.broadcast(a.room_id, me.name .. " drinks a glowing potion.", a.actor_id)
        game.delete_object(a.object_id)
        return string.format("You recover %d HP. (HP: %d/%d)", heal, hp, max_hp)
    end,
}
]=]

-- Locked chest: "unlock chest" (needs the key), then "open chest"
Code.chest = [=[
return {
    unlock = function(a)
        local chest = game.get_object(a.object_id)
        if not chest.metadata.locked then return "It's already unlocked." end
        for _, item in ipairs(game.all_inventory(a.actor_id)) do
            if item.id == chest.metadata.key_id then
                game.update_object(a.object_id, {locked = false})
                game.broadcast(a.room_id, "Click! The chest unlocks.", a.actor_id)
                return "You turn the rusty key. Click!"
            end
        end
        return "The chest is locked. You need a key."
    end,
    open = function(a)
        local chest = game.get_object(a.object_id)
        if chest.metadata.locked then return "The chest is locked." end
        if chest.metadata.looted then return "The chest is empty." end
        if not game.get_object(a.actor_id) then return "Only adventurers can loot." end
        game.update_object(a.object_id, {looted = true})
        local gold = game.roll_dice("3d6") * 10
        game.create_object("/d/crypt/loot/gold-" .. game.time(), "item", a.actor_id, {
            name = gold .. " gold coins",
            description = "Coins stamped with the face of a forgotten king.",
            value = gold,
        })
        return "You lift the lid and scoop up " .. gold .. " gold coins."
    end,
}
]=]

-- Trapped corridor (room code): darts fire a second after you step in
Code.trap = [=[
return {
    on_enter = function(a)
        if a.from == nil then return end
        if game.roll_dice("1d100") > 50 then return end
        game.send(a.actor_id, "Click. You feel a pressure plate sink under your foot!")
        game.call_out(1, "spring", a.actor_id)
    end,
    spring = function(a)
        -- a timer on a room: a.object_id is the room, a.args the victim
        local victim = game.get_object(a.args)
        if not victim or victim.parent_id ~= a.object_id then return end
        local dmg = game.roll_dice("2d6")
        local hp = math.max((victim.metadata.hp or 100) - dmg, 1)
        game.update_object(victim.id, {hp = hp})
        game.send(victim.id, string.format("Poison darts hit you for %d damage! (HP: %d)", dmg, hp))
        game.broadcast(a.object_id, "Darts hiss out of the walls!", victim.id)
    end,
}
]=]

-- Lair (room code): wake every monster here when someone enters
Code.lair = [=[
return {
    on_enter = function(a)
        for _, npc in ipairs(game.get_living_in(a.object_id)) do
            if npc.class == "npc" and npc.code_hash then
                game.use_object(npc.id, a.actor_id, "wake")
            end
        end
    end,
}
]=]

-- Skeleton: attacks every 3s while players are present, strikes back
-- when hit, and respawns after respawn_secs
Code.skeleton = [=[
return {
    wake = function(a)
        game.set_heart_beat(3000)
    end,
    heart_beat = function(a)
        local me = game.get_object(a.object_id)
        if not me or not me.parent_id then game.remove_heart_beat() return end
        for _, e in ipairs(game.get_living_in(me.parent_id)) do
            if e.class == "player" then
                crypt_strike(me, e.id, me.metadata.damage_dice)
                return
            end
        end
        game.remove_heart_beat()   -- nobody here: back to sleep
    end,
    on_damage = function(a)
        local me = game.get_object(a.object_id)
        if me.parent_id then crypt_strike(me, a.attacker_id, me.metadata.damage_dice) end
    end,
    on_death = function(a)
        game.remove_heart_beat()
        game.broadcast(a.room_id, "The skeleton collapses into a pile of bones.")
        local me = game.get_object(a.object_id)
        game.call_out(me.metadata.respawn_secs or 180, "respawn", a.room_id)
    end,
    respawn = function(a)
        local me = game.get_object(a.object_id)
        game.move_object(a.object_id, a.args)
        game.update_object(a.object_id, {hp = me.metadata.max_hp})
        game.broadcast(a.args, "Bones rattle and reassemble. " .. me.name .. " rises again!")
    end,
}
]=]

-- Boss: strikes back when hit, enrages at half health, drops loot
Code.boss = [=[
return {
    on_damage = function(a)
        local me = game.get_object(a.object_id)
        if not me.parent_id then return end   -- this hit killed him
        local taken = (me.metadata.damage_taken or 0) + a.damage
        local enraged = me.metadata.enraged or taken * 2 >= me.metadata.max_hp
        if enraged and not me.metadata.enraged then
            game.broadcast(a.room_id, 'Valdris roars: "YOU DARE?" Green fire wreathes his blade!')
        end
        game.update_object(a.object_id, {damage_taken = taken, enraged = enraged})
        crypt_strike(me, a.attacker_id, "1d8+2")
        if enraged then crypt_strike(me, a.attacker_id, "2d6") end
    end,
    on_death = function(a)
        game.broadcast(a.room_id, 'Valdris shrieks: "I AM ETERNAL!" and dissolves into shadow.')
        game.create_object("/d/crypt/loot/soulreaver", "weapon", a.room_id, {
            name = "Soulreaver",
            description = "The Crypt Lord's blade, still pulsing with dark energy.",
            damage_dice = "2d6", damage_type = "necrotic", value = 2000,
        })
        game.create_object("/d/crypt/loot/crown", "item", a.room_id, {
            name = "Crown of the Crypt Lord",
            description = "Black iron set with a single blood-red gem.",
            value = 5000,
        })
        game.delete_object(a.object_id)   -- a permanent kill
    end,
}
]=]

function build_crypt()
    game.define_class("healing_potion", {
        parent = "item",
        properties = {heal_dice = {type = "string", default = "2d4+2"}},
    })
    game.define_class("locked_chest", {
        parent = "container",
        properties = {
            locked = {type = "boolean", default = true},
            key_id = {type = "string", default = ""},
            looted = {type = "boolean", default = false},
            fixed  = {type = "boolean", default = true},
        },
    })

    local h = {}
    for name, src in pairs(Code) do h[name] = game.store_code(src) end

    local function make(path, class, parent, props)
        local obj = game.create_object(path, class, parent, props)
        if obj.error then error(path .. ": " .. obj.error) end
        return obj.id
    end
    local function R(slug) return AREA .. "/" .. slug end
    local function room(slug, name, desc, exits, code)
        return make(R(slug), "room", AREA, {name = name, description = desc, exits = exits, code_hash = code})
    end

    make(AREA, "region", nil, {name = "The Cursed Crypt", danger_level = 3})

    room("entrance", "Crypt Entrance",
        "Stone stairs descend into darkness. Faded warnings are carved above the arch.",
        {south = R("antechamber")})
    room("antechamber", "Burial Antechamber",
        "Alcoves line the walls, each holding a corpse in rotting cloth.",
        {north = R("entrance"), west = R("hall")}, h.lair)
    room("hall", "Hall of Bones",
        "The walls are stacked skulls. Passages lead north and south.",
        {east = R("antechamber"), north = R("corridor"), south = R("throne")}, h.lair)
    room("corridor", "Trapped Corridor",
        "A narrow passage. The floor tiles look suspiciously uniform.",
        {south = R("hall"), north = R("treasury")}, h.trap)
    room("treasury", "Crypt Treasury",
        "Dusty coffers and scattered coins. Something feels wrong.",
        {south = R("corridor")})
    room("throne", "Throne of the Crypt Lord",
        "A throne of blackened bone. Ancient runes glow a sickly green.",
        {north = R("hall")}, h.lair)

    local key = make(R("key"), "item", R("entrance"), {
        name = "rusty key", description = "An iron key, rusted but sound.",
    })
    make(R("potion-1"), "healing_potion", R("antechamber"), {
        name = "healing potion", description = "A vial of shimmering red liquid.",
        code_hash = h.potion,
    })
    make(R("potion-2"), "healing_potion", R("treasury"), {
        name = "greater healing potion", description = "The liquid glows like embers.",
        heal_dice = "4d4+4", code_hash = h.potion,
    })
    make(R("chest"), "locked_chest", R("treasury"), {
        name = "iron chest", description = "A heavy iron chest with a rusted lock.",
        key_id = key, code_hash = h.chest,
    })
    make(R("flamebrand"), "weapon", R("treasury"), {
        name = "Flamebrand", description = "Flames dance along the blade.",
        damage_dice = "1d8", damage_type = "fire", value = 1500,
    })

    make(R("skeleton-guard"), "npc", R("antechamber"), {
        name = "skeleton guard", description = "Rusted armor over yellowed bones.",
        hp = 12, max_hp = 12, attack_bonus = 2, damage_dice = "1d4+1",
        respawn_secs = 120, fixed = true, code_hash = h.skeleton,
    })
    make(R("skeleton-warrior"), "npc", R("hall"), {
        name = "skeleton warrior", description = "A tall skeleton with a notched greatsword.",
        hp = 18, max_hp = 18, attack_bonus = 3, damage_dice = "1d6+1",
        respawn_secs = 180, fixed = true, code_hash = h.skeleton,
    })
    make(R("valdris"), "npc", R("throne"), {
        name = "Valdris the Crypt Lord",
        description = "A towering skeleton in black armor, eyes burning with green fire.",
        hp = 40, max_hp = 40, attack_bonus = 5, fixed = true, code_hash = h.boss,
    })

    return "The Cursed Crypt is built. Next: eval return crypt_verbs()"
end

function crypt_verbs()
    local n = 0
    local function add(verb, id)
        if game.add_action(verb, id, verb) then n = n + 1 end
    end
    add("drink", AREA .. "/potion-1")   -- drunk potions are gone; add() just skips them
    add("drink", AREA .. "/potion-2")
    add("unlock", AREA .. "/chest")
    add("open", AREA .. "/chest")
    return n .. " verbs registered"
end
```

### How the pieces work

| Piece | Technique |
|---|---|
| Potions | Object action `drink`, registered from `eval` by `crypt_verbs()`, so it follows the potion into the player's inventory. The handler checks `parent_id` to require carrying it, heals by updating the player object's `hp`, then deletes the potion, which also removes its verb. |
| Chest | Two verbs on one object. Its state (`locked`, `looted`) lives in properties, because code is re-run fresh on every call. The gold is created directly in the player's inventory (parent = `a.actor_id`). |
| Trap | A room `on_enter` hook. It skips spawns (`a.from == nil`) and schedules a `call_out` that passes the victim's ID as the string argument. When the timer fires, it checks the victim is still in the room. |
| Lair rooms | `on_enter` uses `game.use_object(npc, actor, "wake")`. That runs each skeleton's `wake` handler in the skeleton's own context, so its `set_heart_beat` applies to the skeleton, not the room. |
| Skeletons | The heartbeat attacks a player in the room and goes back to sleep when the room is empty. `on_damage` strikes back. `on_death` stops the heartbeat and schedules `respawn`, passing the room ID because the dead NPC is no longer in a room. `respawn` moves it back and restores `hp`. The built-in combat re-reads `hp` on the next fight. |
| Boss | No heartbeat: it fights back from `on_damage`, tracks damage taken in a property, and enrages at half health. `on_death` drops loot into `args.room_id` and deletes the boss. |
| Shared helper | `crypt_strike` is defined in the library, so every monster's code can call it. |

### Playing it

```
> s
Burial Antechamber ...
> take potion
You take healing potion.
> attack skeleton
You hit skeleton guard for 5 damage.
skeleton guard has 7 HP remaining.
skeleton guard hits you for 4 damage! (HP: 96)
> attack skeleton
CRITICAL HIT! You strike skeleton guard for 8 damage!
skeleton guard is slain!
The skeleton collapses into a pile of bones.
> drink potion
You recover 6 HP. (HP: 100/100)
```

The actual numbers depend on the dice.

---

## Troubleshooting

| Symptom | Cause |
|---|---|
| `eval` shows nothing useful | `print` is a no-op. `return` a value, and build a string from tables (a table shows as `[table]`). |
| `eval` line does nothing after a certain point | A `--` comment swallowed the rest of the line. Remove comments when joining lines. |
| `call_out` returns `""` | No object context (you're in `eval`), or the object already has 32 pending call_outs. |
| `update_object: permission denied` | The acting user can't modify that object. In object code the acting user is the object's **owner**, not the player. |
| `only the owner or a superior may attach code` | You tried to set `code_hash` on an object owned by someone of your level or higher. |
| `database query limit exceeded (101 > 100)` | Object code may make 100 `game.*` database calls per run. Loop less, or cache results in locals. |
| Verbs answer `Unknown command` after a restart | Actions are memory-only. Run your registration function again (`crypt_verbs()`). |
| A heartbeat stopped after a restart | Heartbeats are memory-only. Restart them from a hook, as the lair rooms do. |
| `attack` says "You can't attack ..." | The target's class is neither `npc` (or a class derived from it) nor `monster`. |
| `game.environment(id)` is `nil` in `on_death` | The dead NPC was already removed from the room. Use `args.room_id`. |
| A player is moved with `game.move_object` but still sees the old room | Lua moves change only the database, not a connected player's session. Don't teleport players from Lua. |

Limits per run:

| | Object code | `eval` / `run_script` |
|---|---|---|
| Instructions | 1M | 10M |
| Timeout | 500 ms | 5 s |
| DB calls | 100 | 10,000 |
| Venice AI calls | 5 | 5 |
| Memory | 64 MB | 64 MB |

See [Sandbox limits](langref.md#sandbox-limits).

---

## Not yet supported

These are planned or partial; see the [roadmap](../../docs/roadmap.md).
Don't build on them yet:

- **Handlers that never fire on their own:** `on_init`, `on_create`, `on_destroy`, `on_move`, `on_use`, `ai_idle_tick` and `ai_combat_tick`. Call them with `game.use_object` if you need them, except `on_create` and `on_destroy`, which `use_object` refuses.
- **NPC behaviour:** `aggro` and `respawn_time` do nothing, and NPCs don't fight back in the built-in combat. Script it as in Part B.
- **Commands and combat:** there are no `use`, `give` or `flee` player commands (add verbs instead), no PvP, and no Lua access to the combat engine's damage types and effects.
- **Movement from Lua:** `game.move_object` fires no hooks and can't teleport connected players.
- **Persistence:** custom verbs, heartbeats and classes defined with `define_class` don't survive a restart.
- **Universe config:** keys like `pvp_mode` or `spawn_room` have no effect. Use `setportal` for the spawn room.

---

## Summary

You've learned how to:

- build rooms, items and NPCs with `game.create_object`, and organise them under one path prefix
- define classes for shared defaults
- attach object code with `code_hash`, and that it always runs as the object's owner
- react to players with room and NPC hooks, and add verbs with `game.add_action`
- schedule work with `call_out` and heartbeats from inside object code
- talk to players with `send`, `broadcast` and `broadcast_region`
- grant builders their own areas, and test as them with `set_actor`

For every function and its exact behaviour, see the
[Lua API reference](langref.md).
