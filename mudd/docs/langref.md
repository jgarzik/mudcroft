# Mudcroft Lua API Reference

This is the reference for the `game.*` Lua API that Mudcroft scripts use.
For a guided introduction, see the [tutorial](langtutor.md). For access
levels and path grants, see [permissions](permissions.md).

- [Where Lua runs](#where-lua-runs)
- [Execution model](#execution-model)
- [Function index](#function-index)
- [Objects](#objects)
- [Object code and handlers](#object-code-and-handlers)
- [Classes](#classes)
- [Queries](#queries)
- [Custom verbs](#custom-verbs)
- [Messaging](#messaging)
- [Timers](#timers)
- [Permissions](#permissions)
- [Credits](#credits)
- [Venice AI](#venice-ai)
- [Utilities](#utilities)
- [Core library](#core-library)
- [Built-in classes](#built-in-classes)
- [Built-in combat](#built-in-combat)
- [Not yet supported](#not-yet-supported)
- [Lua quick reference](#lua-quick-reference)

---

## Where Lua runs

| Entry point | Who can start it | Runs as | Limits |
|---|---|---|---|
| `eval <lua>` command | wizard+ | the wizard | eval |
| `POST /universe/{id}/run_script` (see [API](API.md)) | admin+ or the universe owner | the caller, or an account below an admin caller | eval |
| Object hooks (`on_enter`, `on_damage`, ...) | gameplay | the object's **owner** | object code |
| Custom verbs (`game.add_action`) | a player typing the verb | the object's **owner** | object code |
| Timers (`call_out`, `heart_beat`) | the timer loop | the object's **owner** | object code |
| `game.use_object` | another script | the used object's **owner** | object code |
| Universe libraries | loaded before every script above | the same identity as that script | — |

Builders can't run Lua directly. `eval` requires wizard, and
`game.store_code` requires wizard. Builders build with the `create`
command and own what they create. See [Built-in commands](langtutor.md#built-in-commands).

---

## Execution model

### Identity: object code runs as its owner

Object code always runs as the object's **owner** (the account in
`owner_id`). This is the LPC "effective user" model. The player who
triggered the code is passed in as data (`args.actor_id`), so their
privileges are never used.

- An object owned by a wizard can change any object.
- An object owned by a builder can change only what that builder owns or holds a path grant for.
- An object with no owner runs with no user, so every permission-checked write fails.

`game.create_object` and `game.clone_object` set the new object's owner to
the acting user. Details: [Who a script runs as](permissions.md#who-a-script-runs-as).

`actor_id` is a **player object ID** such as `/players/p-<account-id>`, not
an account ID. To find the account, read the player object's `owner_id`:

```lua
local account_id = game.get_object(args.actor_id).owner_id
```

### Contexts

Each execution may have a **room context** and an **object context**:

| Execution | Room context | Object context |
|---|---|---|
| `eval`, `run_script` | none | none |
| Hook | the room where it happened | the hooked object |
| Custom verb | the player's room | the verb's object |
| Timer callback | the object's `parent_id` | the object |
| `game.use_object` | the used object's `parent_id` | the used object |

The context decides how some functions behave:

- **Room context:** `game.add_action` registers a *room* action. Without a room context it registers an *object* action.
- **Object context:** `game.call_out`, `game.remove_call_out`, `game.set_heart_beat` and `game.remove_heart_beat` need one. Without it they do nothing (and return `""` or `false`).

### Output

- `print` is a **no-op**. To see a value, `return` it or `game.send` it.
- `eval` shows the chunk's return value as text. Strings, numbers and booleans print as-is, `nil` prints `nil`, and a table prints `[table]`. Return a string to see table contents.
- `eval` accepts a bare expression too: `eval 1 + 2` prints `3`.
- The web client sends one line at a time. Separate statements with spaces or `;`.
- Messages from `game.send`, `game.broadcast` and `game.broadcast_region` are queued and delivered **after the script finishes**, in the order they were queued. A script that raises an error delivers none of them (see [One run, one change](#one-run-one-change)).
- A custom verb's return value is the player's reply. See [Custom verbs](#custom-verbs).

### One run, one change

Each run (an `eval`, a script, a handler or hook, with everything it calls through `game.use_object`) is **one change**, and so is each command a player types, with every hook and verb handler it triggers: its writes are saved together, as one Raft entry, when it finishes without error. If it raises an error (or runs out of time or budget), **nothing it did is kept**: no object, property, verb, timer, effect or credit change, and no message is sent. A handler or hook that fails inside a command undoes only its own changes; the rest of the command stands. A command whose reply is an error keeps nothing. Within a run, reads see the run's own earlier writes. An error caught with `pcall` does not end the run; the writes made before it stand unless the run as a whole fails.

The one exception is a Venice call (`game.llm_chat`, `game.llm_image`), which is slow and outside the database: it is a **commit point**. The run's writes before it are saved as one change, its price (`venice_chat_cost`, `venice_image_cost`) is charged (and refunded if the call fails), and the rest of the run is a new change: an error after the call undoes only what came after it. To keep a run all-or-nothing, make Venice calls before changing anything.

### Sandbox limits

Limits apply per execution. Each hook, verb, timer callback and `use_object`
call is one execution, and one budget covers all of it: the universe
libraries, the object code and the handler together.

| Limit | Object code | `eval` and `run_script` |
|---|---|---|
| Lua instructions | 1,000,000 | 10,000,000 |
| Wall-clock timeout | 500 ms | 5 s |
| Memory | 64 MB | 64 MB |
| Database-backed `game.*` calls | 100 | 10,000 |
| Venice AI calls (`llm_chat`, `llm_image`) | 5 | 5 |

Exceeding a limit raises an error, for example `database query limit exceeded (101 > 100)`.

Every limit stops a script as soon as it is exceeded, while it runs: the
instruction count and the wall-clock timeout are both checked every 1,000
instructions. They hold even for `while true do end`, and `pcall`/`xpcall`
cannot catch them (once a limit is hit they re-raise instead of returning
`false`). Time spent inside a single `game.*` call is not interrupted, but
it counts: the deadline is checked again when control returns to Lua and
when the run ends. A `game.use_object` call runs within its caller's
deadline, so nesting never adds time.

Every `game.*` function counts as a database call except these:
`add_action`, `remove_action`, `send`, `broadcast`, `broadcast_region`,
`get_actor`, `can_access_path`, `time` and `roll_dice`. The Venice
functions count against the Venice limit instead. The global
`parent()` also counts as a database call.

Each `game.use_object` call runs in a fresh sandbox with its own
object-code budget. One top-level execution may make at most 32
`use_object` calls in total, counted across all nesting levels.

### Sandbox globals

- **Available:** `string`, `table`, `math`, `utf8`, the Lua base functions (`pairs`, `ipairs`, `pcall`, `error`, `tostring`, `type`, ...), and `print`, which does nothing. `pcall` and `xpcall` are the sandbox's own: they catch ordinary errors but pass the error to you (or to the `xpcall` handler) as a string, even if `error` was given a table.
- **Removed:** `os`, `io`, `load`, `loadstring`, `loadfile`, `dofile`, `require`, `package`, `debug`, `collectgarbage`.
- **Added:** the `game` table, the [`parent`](#parentclass_name-handler_name-args) function, and any globals defined by the universe's libraries (see [Core library](#core-library)).

The interpreter is Lua 5.4.

---

## Function index

Permission requirements apply to the **acting user**: the object's owner
for object code, or the `game.set_actor` override if one is set.

| Function | Returns | Requirement |
|---|---|---|
| [`create_object(path, class, parent_id, props)`](#gamecreate_objectpath-class-parent_id-props) | object or `{error}` | create rights at `path` |
| [`get_object(id)`](#gameget_objectid) | object or `nil` | — |
| [`update_object(id, changes)`](#gameupdate_objectid-changes) | `true`, or `false` if not found | Modify; owner or superior for `code_hash` |
| [`delete_object(id)`](#gamedelete_objectid) | `true`, or `false` if not found | Delete |
| [`move_object(id, new_parent_id)`](#gamemove_objectid-new_parent_id) | `true` | Move |
| [`clone_object(id, new_path, new_parent_id)`](#gameclone_objectid-new_path-new_parent_id) | object, `nil` or `{error}` | create rights at `new_path` |
| [`store_code(source)`](#gamestore_codesource) | hash | wizard+ |
| [`get_code(hash)`](#gameget_codehash) | source or `nil` | — |
| [`define_class(name, definition)`](#gamedefine_classname-definition) | `true` | wizard+ |
| [`get_class(name)`](#gameget_classname) | table or `nil` | — |
| [`is_a(obj_id, class_name)`](#gameis_aobj_id-class_name) | boolean | — |
| [`get_class_chain(class_name)`](#gameget_class_chainclass_name) | array | — |
| [`environment(obj_id)`](#gameenvironmentobj_id) | object or `nil` | — |
| [`all_inventory(obj_id)`](#gameall_inventoryobj_id) | array | — |
| [`get_children(parent_id, filter)`](#gameget_childrenparent_id-filter) | array | — |
| [`present(name, env_id)`](#gamepresentname-env_id) | object or `nil` | — |
| [`get_living_in(env_id)`](#gameget_living_inenv_id) | array | — |
| [`add_action(verb, object_id, method)`](#gameadd_actionverb-object_id-method) | boolean | Modify, and owner or wizard+ outranking the owner |
| [`remove_action(verb, object_id)`](#gameremove_actionverb-object_id) | boolean | Modify, and owner or wizard+ outranking the owner |
| [`send(player_id, message)`](#gamesendplayer_id-message) | `true` | — |
| [`broadcast(room_id, message, exclude_id)`](#gamebroadcastroom_id-message-exclude_id) | `true` | — |
| [`broadcast_region(region_id, message)`](#gamebroadcast_regionregion_id-message) | `true` | — |
| [`call_out(delay_secs, method, args)`](#gamecall_outdelay_secs-method-args) | timer ID or `""` | object context |
| [`remove_call_out(timer_id)`](#gameremove_call_outtimer_id) | boolean | object context; own timers only |
| [`set_heart_beat(interval_ms)`](#gameset_heart_beatinterval_ms) | boolean | object context |
| [`remove_heart_beat()`](#gameremove_heart_beat) | boolean | object context |
| [`apply_effect(target_id, type, duration, magnitude, damage_type, period)`](#gameapply_effecttarget_id-type-duration-magnitude-damage_type-period) | `true` | Modify on the target |
| [`remove_effect(target_id, type)`](#gameremove_effecttarget_id-type) | boolean | Modify on the target |
| [`get_effects(target_id)`](#gameget_effectstarget_id) | list of effects | — |
| [`can_attack(attacker_id, target_id)`](#gamecan_attackattacker_id-target_id) | `true` or `false, reason` | — |
| [`attack(attacker_id, target_id)`](#gameattackattacker_id-target_id) | blow table or `nil, reason` | control the attacker (players: themselves, never object code) |
| [`combat_state(id)`](#gamecombat_stateid) | state table | — |
| [`damage(target_id, amount, type, source_id)`](#gamedamagetarget_id-amount-type-source_id) | `{damage, hp, killed}` or `nil, reason` | Modify on the target |
| [`heal(target_id, amount)`](#gamehealtarget_id-amount) | new HP or `nil, reason` | Modify on the target |
| [`stop_combat(id)`](#gamestop_combatid) | fighters stopped or `nil, reason` | Modify on it |
| [`flee(id)`](#gamefleeid) | direction or `nil, reason` | Move on it (players: themselves, never object code) |
| [`set_actor(account_id)`](#gameset_actoraccount_id) | boolean | wizard+, target below caller |
| [`get_actor()`](#gameget_actor) | account ID or `nil` | — |
| [`check_permission(action, target_id, is_fixed, owner_id)`](#gamecheck_permissionaction-target_id-is_fixed-owner_id) | `{allowed, error}` | — |
| [`can_access_path(path)`](#gamecan_access_pathpath) | boolean | — |
| [`get_access_level(account_id)`](#gameget_access_levelaccount_id) | level string | — |
| [`set_access_level(account_id, level)`](#gameset_access_levelaccount_id-level) | boolean | admin+ (see rules) |
| [`grant_path(grantee_id, path_prefix, can_delegate)`](#gamegrant_pathgrantee_id-path_prefix-can_delegate) | grant or `{error}` | wizard+, or a delegable grant |
| [`revoke_path(grant_id)`](#gamerevoke_pathgrant_id) | boolean or `{error}` | wizard+, the grantor, or a delegable grant |
| [`get_path_grants(account_id)`](#gameget_path_grantsaccount_id) | array | — |
| [`get_credits()`](#gameget_credits) | integer | — |
| [`object_credits()`](#gameobject_credits) | integer | — |
| [`request_payment(amount, reason, handler, data)`](#gamerequest_paymentamount-reason-handler-data) | request id | a paying player |
| [`deduct_credits(amount, reason, payer)`](#gamededuct_creditsamount-reason-payer) | boolean | — |
| [`transfer_credits(from, to, amount)`](#gametransfer_creditsfrom-to-amount) | boolean | — |
| [`admin_grant_credits(holder, amount)`](#gameadmin_grant_creditsholder-amount) | boolean | admin+ |
| [`llm_chat(messages, tier, opts)`](#gamellm_chatmessages-tier-opts) | string or `{error}` | Venice configured |
| [`llm_image(prompt, style, size, opts)`](#gamellm_imageprompt-style-size-opts) | image hash or `{error}` | Venice configured |
| [`time()`](#gametime) | milliseconds | — |
| [`set_time(time_ms)`](#gameset_timetime_ms) | boolean | wizard+ |
| [`roll_dice(dice)`](#gameroll_dicedice) | integer | — |
| [`set_rng_seed(seed)`](#gameset_rng_seedseed) | boolean | wizard+ |
| [`use_object(obj_id, actor_id, verb, target_id)`](#gameuse_objectobj_id-actor_id-verb-target_id) | handler result or `nil` | — |
| [`get_universe()`](#gameget_universe) | table or `nil` | — |
| [`update_universe(config)`](#gameupdate_universeconfig) | boolean | admin+ |
| [`parent(class_name, handler_name, args)`](#parentclass_name-handler_name-args) (global) | handler result or `nil` | — |

---

## Objects

### Object paths

Every object ID is a filesystem-style path such as `/d/forest/clearing`.

- A path starts with `/`.
- Each segment matches `[a-z][a-z0-9-]*`: it starts with a letter, followed by lowercase letters, digits or hyphens.
- A path is at most 255 characters.
- Uppercase letters are converted to lowercase.

Object IDs are scoped per universe: two universes may both have
`/rooms/start`, and every `game.*` function reads and writes only the
script's own universe. An account has its own player object
(`/players/p-<account>`, with its own inventory and HP) in each universe it
visits. Scripts cannot create player objects: `create_object` and
`clone_object` refuse class `player` and paths under `/players/`.

Path grants work on prefixes, so group an area under one prefix such as
`/d/crypt/...`. Then a single grant covers the whole area.

### Object tables

Functions that return objects return tables like this:

| Field | Meaning |
|---|---|
| `id` | the object's path |
| `universe_id` | the universe it belongs to |
| `class` | class name |
| `parent_id` | containing object (room, container, player), or `nil` |
| `owner_id` | owning account ID, or `nil` |
| `code_hash` | attached code hash, or `nil` |
| `name`, `description` | copied to the top level for convenience |
| `metadata` | every other property, e.g. `obj.metadata.hp`, `obj.metadata.exits` |

The table is a copy. Changing it does nothing; use `game.update_object`
to write changes.

#### `game.create_object(path, class, parent_id, props)`

Creates and saves an object. The acting user becomes its owner.

- `path` (string): the new object's ID.
- `class` (string): its class. The server doesn't check that the class exists.
- `parent_id` (string or nil): its container. The container must exist.
- `props` (table or nil): property values. These override the class defaults.

The new object starts with the default properties of its class and every
ancestor class, then `props` are applied on top. Two kinds of default are
skipped: empty-string defaults (such as the root class's `name` and
`description`), and a class's `code_hash` default.

If `props.code_hash` is set, it attaches that stored code instead of
becoming a property. This is the only way `create_object` attaches code.
See [Attaching code](#attaching-code).

```lua
local sword = game.create_object("/d/keep/armory/sword", "weapon", "/d/keep/armory", {
    name = "Iron Sword",
    description = "A sturdy iron blade.",
    damage_dice = "1d8",
})
if sword.error then return sword.error end
return sword.id   -- "/d/keep/armory/sword"
```

**Returns** the new object, or `{error = "..."}` in these cases:

- the path is invalid
- the acting user can't create at that path
- `code_hash` is not a string, or is not a known hash

Other database errors, such as a duplicate path or a missing parent, raise
a Lua error.

#### `game.get_object(id)`

**Returns** the object, or `nil` if it doesn't exist.

```lua
local npc = game.get_object("/d/keep/guard")
return npc and npc.metadata.hp
```

#### `game.update_object(id, changes)`

Merges `changes` into the object's properties. Each top-level key replaces
the old value: `{exits = {north = ...}}` replaces the whole `exits` table. Only the
keys you pass are written (in place, in one atomic change), so a property
changed by something else meanwhile, such as `hp` in combat, is not put back.
Property names may not contain `"`, `\` or control characters.

A `code_hash` key attaches stored code, or detaches it if set to `nil`. It
is never stored as a property. See [Attaching code](#attaching-code).

```lua
game.update_object("/d/keep/guard", {hp = 40, mood = "angry"})
```

**Returns** `true`, or `false` if the object doesn't exist.

**Raises** an error in these cases:

- the acting user lacks Modify permission (`update_object: permission denied: ...`)
- `code_hash` names an unknown hash or is not a string
- the acting user may not attach code to this object

#### `game.delete_object(id)`

Deletes the object. Deleting also cancels the object's pending
`call_out`s, its heartbeat, and every custom verb registered for it.

An object that still contains other objects can't be deleted. Move or
delete its contents first.

**Returns** `true`, or `false` if the object doesn't exist. **Raises** an
error if the acting user lacks Delete permission.

#### `game.move_object(id, new_parent_id)`

Sets the object's container. Pass `nil` to take it out of any container.
An object can never be put inside itself, directly or through what it
contains: such a move raises an error.

```lua
game.move_object("/d/keep/armory/sword", args.actor_id)   -- into a player's inventory
```

Anyone may move an object that isn't `fixed`. Fixed objects can be moved only by wizards+, their owner, or a holder of a path grant covering them.

After the script ends, a move:

- moves a connected player's session too: the player is shown the new room
  and later commands happen there;
- runs `on_leave` on the old container and `on_enter` on the new one (for
  any moved object, not only players), with `moved_id` set to the moved
  object and `moved_by` to the account the script acted as. `actor_id` is
  **not** set: no player chose to move. Hooks that move things run more
  hooks: they nest at most 4 deep, and one top-level run causes at most 32
  move hooks in total; further moves happen but run no hooks.
- shows a moved player the new room without starting room image
  generation.

**Returns** `true`. **Raises** an error if the object or destination
doesn't exist, either belongs to another universe, or the acting user
lacks Move permission.

#### `game.clone_object(id, new_path, new_parent_id)`

Copies an object to a new path. The copy keeps the original's properties
and `code_hash`, and the acting user becomes its owner.

**Returns** the copy, `nil` if the source doesn't exist, or
`{error = "..."}` if the source belongs to another universe, `new_path` is
invalid, or the acting user can't create there.

---

## Object code and handlers

### `game.store_code(source)`

Stores Lua source in the content-addressed code store. Requires
**wizard+**.

**Returns** the SHA-256 hex hash. The same source always gives the same
hash. Raises an error for non-wizards.

### `game.get_code(hash)`

**Returns** the stored source, or `nil`.

### Writing object code

Object code must evaluate to a table of handler functions. Each handler
takes one `args` table:

```lua
return {
    on_enter = function(args)
        if not args.actor_id then return end   -- an object was moved in
        game.send(args.actor_id, "Cold air rushes past you.")
    end,
    pull = function(args)
        return "You pull the lever."
    end,
}
```

The code is evaluated fresh for every execution, so Lua variables don't
persist between calls. Keep state in object properties with
`game.update_object`.

Code that has no handler for an event is skipped silently.

### Attaching code

Attach stored code by setting `code_hash`:

```lua
local h = game.store_code([[return { on_enter = function(a)
    if a.actor_id then game.send(a.actor_id, "Welcome!") end
end }]])
game.update_object("/d/keep/hall", {code_hash = h})   -- attach to an existing object
game.create_object("/d/keep/bell", "item", "/d/keep/hall", {name = "bell", code_hash = h})   -- or attach at creation
game.update_object("/d/keep/hall", {code_hash = nil}) -- detach
```

- The hash must already exist in the code store. Unknown hashes are rejected.
- With `update_object`, only the object's owner, or an account that outranks the owner, may set `code_hash`. The code will run as the owner, so this stops a wizard from planting code on an admin's object. Ownerless objects have no such restriction.
- A class's `code_hash` property default is used only by [`parent()`](#parentclass_name-handler_name-args). It is never attached to objects created from the class; pass `code_hash` in `props` to attach code.

### When handlers run

| Handler | Fires when | Runs on | `args` fields (all include `object_id`) |
|---|---|---|---|
| `on_enter` | a player walks in, arrives by `goto`, or connects and spawns there; or `game.move_object` moves anything into it | the room (or container) | `moved_id` (what arrived), `room_id`, `from` (`nil` on spawn), `direction` (walking only); `actor_id` when a player moved themselves, `moved_by` for a Lua move |
| `on_leave` | a player walks out, or leaves by `goto`; or `game.move_object` moves anything out of it | the room (or container) | `moved_id` (what left), `room_id`, `to`, `direction` (walking only); `actor_id` when a player moved themselves, `moved_by` for a Lua move |
| `on_damage` | the `attack` command hits | the target NPC | `actor_id`, `room_id`, `attacker_id`, `damage` |
| `on_death` | the `attack` command kills | the target NPC | `actor_id`, `room_id`, `killer_id` |
| `heart_beat` | the heartbeat set by `game.set_heart_beat` is due | the object | `room_id`, `args` (always `nil`) |
| any name | a `call_out` is due | the object | `room_id`, `args` (the string given to `call_out`, or `nil`) |
| any name | a custom verb is typed | the verb's object | `actor_id`, `room_id`, `verb`, `args` (the rest of the line) |
| any name | `game.use_object` is called | the used object | `actor_id`, `verb`, `target_id`, `code_hash` |

`room_id` is the room the event happened in. For timers it is the object's
`parent_id` at the moment the timer fires.

Hook details:

- **Room hooks:** `on_enter` and `on_leave` run only on the room's code.
  - `on_leave` runs before the player moves. A broadcast to that room still reaches the leaving player unless you exclude them.
  - `on_enter` runs after the move and after the player has seen the new room's description.
- **Combat hooks:** `on_damage` and `on_death` run after the attacker's combat report. A killing blow fires both.
  - By the time `on_death` runs, the dead NPC has been removed from the room (`parent_id = nil`). Use `args.room_id`, not `game.environment`.
- **Return values:** hook return values are ignored. Errors are logged on the server.
- **Moves from Lua:** `game.move_object` runs `on_leave`/`on_enter` after the script ends, with `moved_id` and `moved_by` but no `actor_id`, so a hook that rewards or punishes `actor_id` fires only for a player's own move.
  - This includes the core `take`, `drop` and `give` commands, which move items with `game.move_object`: dropping a sword runs the room's `on_enter` with `moved_id` = the sword. **Room hooks must allow `actor_id` to be `nil`** (`if not a.actor_id then return end` for player-only effects); `game.send(nil, ...)` raises an error.
- **Unwired hooks:** region objects get no hooks. The class handler names `on_create`, `on_destroy`, `on_init`, `on_move` and `on_use` are **never fired by the server**. You can still call all of them except `on_create` and `on_destroy` with `game.use_object`. `ai_idle_tick` and `ai_combat_tick` are fired for NPCs (see [Built-in combat](#built-in-combat)).

### `parent(class_name, handler_name, args)`

A global function, not part of `game`, for class-based code. It finds
the parent class of `class_name`, loads that class's code from its
`code_hash` property default, and calls `handler_name(args)`.

The parent's code runs in the current sandbox with the current identity.

**Returns** the handler's result, or `nil` in any of these cases:

- the class has no parent
- the parent class has no code
- the parent's code has no such handler

```lua
-- code attached to class "fire_sword" (parent "weapon")
return {
    on_hit = function(args)
        local base = parent("fire_sword", "on_hit", args) or 0
        return base + game.roll_dice("1d6")
    end,
}
```

---

## Classes

Classes are shared by **every universe** on the server.

### `game.define_class(name, definition)`

Defines a class, or redefines one you created earlier. Requires
**wizard+**. Built-in classes can't be redefined. An existing custom class
can be redefined only by the account that defined it, or by an admin+.

`definition` takes two fields:

- `parent`: the parent class name.
- `properties`: a table of `{type = "...", default = value}` entries. Only `default` is used; `type` is informational.

```lua
game.define_class("lantern", {
    parent = "item",
    properties = {
        lit  = {type = "boolean", default = false},
        fuel = {type = "number",  default = 100},
    },
})
```

**Returns** `true`. Raises an error for non-wizards, built-in names, or a
class defined by another account (`define_class: <name> is already defined
by another account`).

Custom classes are currently **not saved** to the database: the server's
attempt to persist them fails. They are lost when the server restarts.
Objects keep the properties they were created with. Run your
`define_class` calls again after a restart if you create more objects
from those classes.

### `game.get_class(name)`

**Returns** `{name, parent, properties, handlers}` or `nil`.

- `properties` holds only this class's own defaults; inherited ones aren't included.
- `handlers` lists the handler names declared by a built-in class. It is informational.

### `game.is_a(obj_id, class_name)`

**Returns** `true` if the object's class is `class_name` or inherits from
it. Returns `false` if the object doesn't exist.

### `game.get_class_chain(class_name)`

**Returns** the inheritance chain from the class up to the root, for
example `{"weapon", "item", "thing"}`.

---

## Queries

#### `game.environment(obj_id)`

**Returns** the object's container (its `parent_id` object), or `nil`.

#### `game.all_inventory(obj_id)`

**Returns** an array of every object whose `parent_id` is `obj_id`.

#### `game.get_children(parent_id, filter)`

Like `all_inventory`, with an optional class filter `{class = "name"}`.
The filter matches the class name exactly; subclasses don't match.
`parent_id` is required.

```lua
local rooms = game.get_children("/d/crypt", {class = "room"})
```

#### `game.present(name, env_id)`

Finds an object directly inside `env_id` whose `name` contains `name`,
ignoring case. `%` and `_` in `name` match literally. If several match,
which one you get is unspecified.

**Returns** the object or `nil`.

#### `game.get_living_in(env_id)`

**Returns** objects in `env_id` whose class is `living` or descends from it:
players, NPCs and their subclasses (including classes made with
`define_class`).

---

## Custom verbs

Objects can add verbs such as `pull lever`. When a player types a command
that isn't built in, the server looks for a registered verb.

#### `game.add_action(verb, object_id, method)`

Registers `verb` to call handler `method` on `object_id`'s code. The
handler will run as the object's owner, so the acting user needs
**Modify** permission on the object **and** must be its owner or a
wizard+ who outranks the owner (the same rule as attaching code).
Ownerless objects need only Modify.

`method` can't be private: names starting with `_` and the engine hooks
(`on_enter`, `on_leave`, `on_damage`, `on_death`, `heart_beat`,
`on_create`, `on_destroy`) are refused.

Where the action is registered depends on the script's context:

- **With a room context** (hooks, verbs, timers): it becomes a *room action* for that room.
- **Without one** (`eval`, `run_script`): it becomes an *object action* that follows the object wherever it goes.

**Returns** `true` if registered, or `false` if permission is denied or
`method` is private.

```lua
game.add_action("pull", "/d/keep/hall/lever", "pull")
```

When several nearby objects offer the verb, the one whose name the player
typed wins: an exact name first, then a name the words contain (or that
contains them). The player is the handler's **paying player** (see
[Credits](#credits)) only if they clearly picked the object: it is the only
one offering the verb, they typed its exact name, or only one name matched.
Otherwise the handler runs with no paying player, so a look-alike object
cannot capture a verb and bill someone.

#### `game.remove_action(verb, object_id)`

Removes the action from the same scope that `add_action` would use in
this context. It needs the same permission as `add_action`. **Returns**
`true`, or `false` if permission is denied.

### How a typed verb is dispatched

1. Built-in commands always win. These verbs can't be overridden: `look`, `l`, the directions and their abbreviations, `say`, `help`, `get`, `take`, `drop`, `inventory`, `inv`, `i`, `attack`, `kill`, `goto`, `setportal`, `eval` and `create`. For the "soft" built-ins `wield`, `wear`, `unwield`, `remove`, `equipment`, `eq` and `get`/`take ... from ...`, a matching custom verb (below) comes first and the built-in runs only if none matches; such a handler never gets the player as payer (they may have meant the built-in). Safety commands (`pvp`, `flee`) can't be shadowed.
2. The first word, lowercased, is the verb. The rest of the line is `args`.
3. Candidates are collected in this order:
   - room actions registered for the player's room, but only for objects that are nearby;
   - object actions on items in the player's inventory;
   - object actions on objects in the room;
   - object actions on the room itself.

   "Nearby" means one of the player's inventory items, an object in the room, or the room itself.
4. If the player typed more after the verb, the first candidate whose object `name` matches the text is chosen. The match works either way: the name contains the text, or the text contains the name. Otherwise the first candidate wins.
5. The handler runs as the object's owner with `{verb, args, actor_id, object_id, room_id}`. Actions whose method is private (`_name` or an engine hook) are never dispatched.
6. The handler's return value is the reply. Return a string. Returning `nil` sends no reply, which is useful if the handler already used `game.send`.
7. If nothing matches, the player sees `Unknown command: <verb>`.
8. If the handler raises an error, players see `Something went wrong.` and wizards+ see the error text.

Actions are stored in the database: they survive restarts (and failovers)
and are removed with their object, or their room. Registering the same
action again replaces its method.

---

## Messaging

Messages are delivered after the script finishes. They reach only
connected players in the script's universe: `game.send` and
`game.broadcast_region` skip players connected to other universes, and
`game.broadcast` to a room of another universe is dropped.

#### `game.send(player_id, message)`

Sends `message` to one player. `player_id` is a player object ID such as
`args.actor_id`.

#### `game.broadcast(room_id, message, exclude_id)`

Sends `message` to every player in the room, except `exclude_id` if given.

```lua
game.send(args.actor_id, "You ring the bell.")
game.broadcast(args.room_id, "Someone rings the bell.", args.actor_id)
```

#### `game.broadcast_region(region_id, message)`

Sends `message` to every player whose room matches either rule:

- the room's path is under `region_id` (e.g. `/d/crypt/hall` for region `/d/crypt`);
- the room's `parent_id` is `region_id`.

All three functions return `true`.

---

## Timers

Timers need an **object context**, so call them from object code. The
callback runs that object's code, as its owner.

#### `game.call_out(delay_secs, method, args)`

Schedules handler `method` on the current object after `delay_secs`
seconds (fractions allowed; at least 0.1 s, at most 30 days).

- `args` is an optional **string**. It arrives as `args.args` in the callback. To pass structured data, encode it in a string or store it in a property.
- The callback receives `{object_id, room_id, args}`, where `room_id` is the object's `parent_id` when the timer fires.
- The timer loop runs every 100 ms, so timing is accurate to about 0.1 s.
- Each object may have at most **32** pending call_outs.

**Returns** a timer ID, or `""` in two cases: there is no object context
(for example in `eval`), or the object already has 32 pending call_outs.

```lua
return {
    light_fuse = function(a)
        game.call_out(5, "explode", a.actor_id)
        return "The fuse starts to hiss."
    end,
    explode = function(a)
        game.broadcast(a.room_id, "BOOM!")
        game.send(a.args, "You were standing far too close.")
    end,
}
```

#### `game.remove_call_out(timer_id)`

Cancels one of the current object's own pending call_outs. **Returns**
`true` if it was pending, `false` if it was not, belongs to another object,
or there is no object context.

#### `game.set_heart_beat(interval_ms)`

Calls the current object's `heart_beat` handler every `interval_ms`
milliseconds. Calling it again replaces the interval. **Returns** `true`,
or `false` without an object context.

- The interval is clamped to 100 ms through one day.
- The callback receives `{object_id, room_id, args = nil}`.
- Heartbeats are stored and resume after a server restart (the first beat one interval after startup).

**Returns** `true`. Without an object context it does nothing.

#### `game.remove_heart_beat()`

Stops the current object's heartbeat. **Returns** `true` if one was
running.

Pending call_outs and heartbeats are saved to the database and reloaded at startup.

Deleting an object cancels both its call_outs and its heartbeat.

## Status effects

Status effects (poisoned, stunned, bleeding, ...) are stored in the
`active_effects` table, one row per effect type on an object, so they
survive a restart. Deleting an object deletes its effects, and a combatant
killed by `attack` or by its effects loses them.

Effects run on deadlines, not a fixed tick. A plain effect (stunned,
weakened, ...) is simply in force until its end time. A **periodic** effect
pulses every `period` seconds until it ends (a pulse due exactly at the end
still happens): damage over time (`poisoned`, `burning`, `bleeding`, or any
effect given a damage type) deals its magnitude in damage per pulse, after
the target's resistances, and `regenerating` heals it (up to `max_hp`).
Pulses change the target's `hp` property. A player is told each pulse
(`You take 3 poison damage (poisoned).`) and when an effect ends
(`You are no longer poisoned.`). Damage that brings HP to 0 kills, as any
death does (see [Built-in combat](#built-in-combat): the universe's death
policies, respawns, XP), with `killer_id` set to whoever applied the effect
(an attacker's critical hit causes bleeding), all in one atomic change. Within one pass, all damage is
applied before any healing, and healing cannot revive the dead. If the
server falls behind (or was down), the missed pulses are applied together.

Types: `poisoned`, `stunned`, `blinded`, `burning`, `frozen`, `paralyzed`,
`slowed`, `hasted`, `strengthened`, `weakened`, `protected`, `exposed`,
`invisible`, `regenerating`, `silenced`, `bleeding`.

#### `game.apply_effect(target_id, type, duration, magnitude, damage_type, period)`

Adds an effect lasting `duration` seconds (0.1 to 86,400; fractions allowed)
with strength `magnitude` (-10,000 to 10,000). `damage_type` is optional;
`poisoned`, `burning` and `bleeding` default to poison, fire and physical.
`period` (seconds between pulses, 0.5 to 3,600, default 1) applies to
periodic effects. `duration` and `period` must be finite numbers (`0/0`
raises an error). Applying a type the target already has keeps the later end
and the stronger magnitude, and a periodic effect keeps its pulse timing.
The acting user needs Modify permission on the target.

**Returns** `true`. **Raises** an error for an unknown type or damage type,
a missing target, or a denied permission.

#### `game.remove_effect(target_id, type)`

**Returns** `true` if the target had the effect, `false` if not. Needs
Modify permission on the target.

#### `game.get_effects(target_id)`

**Returns** a list of `{type, remaining, magnitude, damage_type, period}`
tables: `remaining` is whole seconds left, rounded up; `damage_type` and
`period` are `nil` for effects that do not pulse.

### Combat engine

The rules of [Built-in combat](#built-in-combat), callable from Lua (the
`Combat` library wraps these). Each runs in the current run, so a script's
fights, damage and deaths are part of its one change.

#### `game.can_attack(attacker_id, target_id)`

**Returns** `true` if `attacker_id` may fight `target_id` now (combat
setting, a creature or a player the PvP rules allow, both alive and in the
same room), else `false` and the reason.

#### `game.attack(attacker_id, target_id)`

One blow now, and the fight starts (both sides) if it is not on; during a
fight it switches target instead (no extra blow), returning a blow with
only `text` set. The acting user must be able to modify the attacker; a
player attacker only as themselves and never from object code (so no
script can make a player fight).

**Returns** `{hit, critical, fumble, damage, killed, text, parts}`, or
`nil` and the reason (not allowed, stunned, already fighting that target).

#### `game.combat_state(id)`

**Returns** `{in_combat, attacking, attackers, next_round_at}`: whom `id`
attacks, who attacks it (a list), and when its next round is due (ms since
epoch).

#### `game.damage(target_id, amount, type, source_id)`

Deals `amount` (0 to 1,000,000) damage of `type` (default `physical`) after
the target's resistances. Reaching 0 HP is a death (the universe's policy),
with `source_id` (optional) as the killer. Needs Modify permission on the
target.

**Returns** `{damage, hp, killed}` (`damage` as applied), or `nil` and the
reason.

#### `game.heal(target_id, amount)`

Heals up to `max_hp`. Needs Modify permission on the target. **Returns**
the new HP, or `nil` and the reason.

#### `game.stop_combat(id)`

Ends every fight `id` is in, as fighter or target (a peace spell, a
referee). Needs Modify permission on it. **Returns** how many fighters
stopped, or `nil` and the reason.

#### `game.flee(id)`

Leaves through a random exit, ending `id`'s fight (see Fleeing under
[Built-in combat](#built-in-combat)). Needs Move permission; a player only
as themselves, never from object code. **Returns** the direction, or `nil`
and the reason.

---

## Permissions

See [permissions](permissions.md) for the check order and path grants. In
short, levels rank `player < builder < wizard < admin < owner`. Wizards and
above can change any object. Owners can change their own objects. Builders
can change objects under their path grants.

#### `game.set_actor(account_id)`

Makes later calls in this script act as `account_id`.

- Only **wizards+** may switch, and only to accounts they **outrank** in this
  universe; a global admin or owner can be the target only of a higher global
  level (see [permissions.md](permissions.md#access-levels)).
- Switching to yourself is always allowed.
- `nil` clears the override.
- Ownerless object code can't switch.

**Returns** `true` on success, `false` if refused.

The override applies to these functions:

- object writes and `add_action` / `remove_action`
- `store_code` and `define_class`
- `check_permission` and `can_access_path`
- `set_access_level`, `grant_path` and `revoke_path`
- `admin_grant_credits`
- `set_time` and `set_rng_seed`
- `update_universe`

It does not apply to the credit functions or Venice billing.

#### `game.get_actor()`

**Returns** the current override, or `nil` if none is set.

#### `game.check_permission(action, target_id, is_fixed, owner_id)`

Asks whether the acting user may perform `action` on a target described
by the arguments. The object itself is not looked up, so pass its
`fixed` flag and owner yourself.

- `action`: one of `"read"`, `"modify"`, `"move"`, `"delete"`, `"create"`, `"execute"`, `"store_code"`, `"admin_config"`, `"grant_credits"`.
- `target_id`: the object path, used for path-grant checks.
- `is_fixed` (boolean or nil): affects `"move"`.
- `owner_id` (string or nil): the owner account, used for the owner check.

**Returns** `{allowed = true}` or `{allowed = false, error = "reason"}`.
An unknown action returns `allowed = false`.

```lua
local obj = game.get_object(id)
local r = game.check_permission("modify", obj.id, obj.metadata.fixed, obj.owner_id)
if not r.allowed then return "Denied: " .. r.error end
```

#### `game.can_access_path(path)`

**Returns** `true` if the acting user is wizard+, or holds a path grant
covering `path`.

#### `game.get_access_level(account_id)`

**Returns** the account's effective level in this universe (see
[permissions.md](permissions.md#access-levels)): `"player"`, `"builder"`,
`"wizard"`, `"admin"` or `"owner"`. Unknown IDs, including player object
paths, return `"player"`.

#### `game.set_access_level(account_id, level)`

Sets an account's member level in this universe. The caller must be
**admin+**, and the target's current level must be **below** the caller's.
Only an **owner** may grant `admin`. `owner` is not a member level and is
always refused.

**Returns** `true` if changed, `false` if refused, `level` is unknown or the
write failed.

#### `game.grant_path(grantee_id, path_prefix, can_delegate)`

Grants `grantee_id` access to every object under `path_prefix` in this
universe. The caller must be wizard+, or hold a grant covering the prefix
with `can_delegate = true`.

**Returns** the grant `{id, grantee_id, path_prefix, can_delegate,
granted_by, granted_at}` or `{error = "..."}`.

#### `game.revoke_path(grant_id)`

Revokes a grant. Allowed for wizards+, the original grantor, or anyone
with a delegable grant covering the path.

**Returns** `true`, `false` if no such grant exists, or `{error = "..."}`
if refused.

#### `game.get_path_grants(account_id)`

**Returns** an array of that account's grants in this universe, in the
same shape as `grant_path`'s result.

---

## Credits

Balances are per universe and held by an **account** or an **object** (a
room's repair fund, a shop's till).

- **An object** spends its own balance freely (`"object"`).
- **A player's** credits move only with their consent: object code asks
  with `game.request_payment`, the player sees a prompt, and only if they
  accept does anything move (to the asking object). Code can never take a
  player's credits directly (`"actor"` is refused as a payer), and only code
  the player chose to run (a verb they typed, clearly naming the object)
  can ask: hooks such as `on_enter`, timers, heartbeats and code reached
  through `game.use_object` cannot.

Nothing can spend from any other account. Amounts are whole numbers from 1
to 10^12; others raise an error. A spend never overdraws: it either happens
in full or returns `false` with nothing changed.

#### `game.request_payment(amount, reason, handler, data)`

Asks the player who typed this verb to pay `amount` credits to this object.
They are shown `Pay <amount> credits for <reason>?` (a dialog in the web
client; text clients type `yes` or `no`) and have a minute to answer. If they
accept and can afford it, the credits move to this object and its `handler`
runs (as the object's owner) with `{payment_id, amount, reason, data,
actor_id, actor_account}`; its return value is the player's reply. The payment and
the handler are one change: if the handler raises an error (or does not
exist), neither is kept, the player is not charged, and the request is used
up. To give money back after a successful handler, use
`game.transfer_credits("object", args.actor_account, ...)`. Declining, or not answering,
changes nothing. `data` is an optional string (up to 4096 bytes); `reason`
is cut to 200 characters; `handler` cannot be private or an engine hook.

**Returns** the request's id at once (the answer comes later). Raises an
error without a paying player, outside object code, or when the player
already has 5 unanswered requests.

```lua
-- A shop: "buy lamp" asks for 3 credits, and hands over a lamp once paid
buy = function(args)
  game.request_payment(3, "an oil lamp", "sold", args.actor_id)
  return nil  -- the prompt is the reply
end,
sold = function(args)
  game.clone_object("/proto/lamp", args.data)
  return "The shopkeeper hands you a lamp."
end,
```

#### `game.get_credits()`

**Returns** the paying player's balance, or `0` if there is none.

#### `game.object_credits()`

**Returns** the running object's balance. Raises an error outside object
code.

#### `game.deduct_credits(amount, reason, payer)`

Spends `amount` from the object's own balance (`payer` must be
`"object"`; `"actor"` raises an error: use `request_payment`). `reason` is
logged. **Returns** `true`, or `false` if the object has too little.

#### `game.transfer_credits(from, to, amount)`

Moves `amount` from this object (`from` must be `"object"`) to `to`: an
account id, or an object path in this universe (which must exist). Atomic.
**Returns** `true`, or `false` if the object has too little.

#### `game.admin_grant_credits(holder, amount)`

Adds credits to `holder`, an account id or an object path that exists.
Requires **admin+**. **Returns** `true` on success, `false` if refused.

---

## Venice AI

These calls need a Venice API key on the server (see [admin](admin.md))
and count against the Venice limit of 5 calls per execution.

Each call costs the universe config's `venice_chat_cost` or
`venice_image_cost` credits (default 0; an admin sets them with
`game.update_universe`). A priced call is paid by the object
(`opts.payer = "object"`); a player cannot be billed directly (charge them
first with `game.request_payment`). The price is charged before the call and
refunded if the call fails. A paid call with no
one to pay, or a payer short of credits, returns `{error = ...}` without
calling Venice. The rate limit (60 calls a minute) is kept per universe and
code owner, whoever pays.

#### `game.llm_chat(messages, tier, opts)`

- `messages`: an array of `{role = "system"|"user"|"assistant", content = "..."}`.
- `tier` (optional): `"fast"`, `"balanced"` (the default) or `"quality"`.
- `opts` (optional): `{payer = "object"}` for a priced call.

**Returns** the reply text, or `{error = "..."}`.

```lua
local reply = game.llm_chat({
    {role = "system", content = "You are a grumpy dwarf blacksmith. Answer in one sentence."},
    {role = "user", content = args.args},
}, "fast")
if type(reply) == "table" then return "The smith ignores you." end
return "The smith says: " .. reply
```

#### `game.llm_image(prompt, style, size, opts)`

- `style`: `"realistic"` (the default), `"anime"`, `"digital"` or `"painterly"`.
- `size`: `"small"`, `"medium"` (the default) or `"large"`.
- `opts` (optional): `{payer = "object"}` for a priced call.

**Returns** an image **hash** string, or `{error = "..."}`. The image is
served at `/images/{hash}`.

To set a room's picture, store the hash as `image_hash`; the web client
displays it:

```lua
local h = game.llm_image("A torch-lit crypt entrance", "painterly")
if type(h) == "string" then game.update_object(args.room_id, {image_hash = h}) end
```

---

## Utilities

#### `game.time()`

**Returns** the current time in milliseconds since the Unix epoch, or the
`set_time` override if one is set.

#### `game.set_time(time_ms)`

Overrides `game.time()` for the rest of **this execution**, including
nested `use_object` calls. `0` restores real time. Requires **wizard+**.
**Returns** `true`, or `false` if refused.

#### `game.roll_dice(dice)`

Rolls dice in `NdM`, `NdM+K` or `NdM-K` notation, for example `"2d6+3"`
or `"d20"`. N is 1–100 (default 1) and M is 1–1000.

**Returns** the total. Raises an error for invalid notation.

#### `game.set_rng_seed(seed)`

Seeds `game.roll_dice` for the rest of **this execution**. Use it for
reproducible tests. Requires **wizard+**. **Returns** `true`, or `false`
if refused. Doesn't affect `math.random`.

#### `game.use_object(obj_id, actor_id, verb, target_id)`

Calls a handler on another object's code.

- `verb` picks the handler: `"use"` runs `on_use`, `"hit"` runs `on_hit`, `"look"` runs `on_look`, `"init"` runs `on_init`. Any other verb is used as the handler name.
- The handler gets `{object_id, actor_id, verb, code_hash, target_id}`.
- It runs in a **fresh sandbox, as that object's owner**, with object-code limits. Its object context is that object, and its room context is the object's `parent_id`.
- The caller's identity is never passed on.
- `actor_id` is whatever the caller passed. Handlers must treat it as untrusted: it need not be the player who acted.
- Private handlers can't be called: names starting with `_`, and the engine hooks `on_enter`, `on_leave`, `on_damage`, `on_death`, `heart_beat`, `on_create` and `on_destroy`. Such calls return `nil`.
- Nesting is limited to 8 levels, and one top-level execution may make at most 32 `use_object` calls in total, across all levels. Call 33 raises an error.

**Returns** the handler's result as plain data: tables, strings, numbers
and booleans. Functions don't cross back. Returns `nil` if the object
doesn't exist, has no code, has no such handler, or the handler is private.
Raises an error if the handler fails.

```lua
-- wake the guard when someone enters (in the room's on_enter)
game.use_object("/d/keep/guard", args.actor_id, "wake")
```

#### `game.get_universe()`

**Returns** `{id, name, owner_id, config, created_at}` for the current
universe.

#### `game.update_universe(config)`

Merges the top-level keys of `config` into the universe config. Requires
**admin+**. **Returns** `true`, or `false` if refused.

The server reads these config keys: `lib_hashes`, which maps library
names to code hashes (see [Core library](#core-library)), the Venice
prices `venice_chat_cost` and `venice_image_cost` (see [Venice AI](#venice-ai)), and the
combat settings `combat`, `combat_round_ms`, `pvp_policy`, `flee_fail_percent`, `player_death`, `npc_death` and `corpse_decay_s` (see
[Built-in combat](#built-in-combat)). Other keys are
yours to read with `get_universe()`. Use the `setportal` command to set
the spawn room; config keys don't affect it.

---

## Core library

Every universe has a set of libraries: Lua code that runs before **every**
script in that universe.

- **Core libraries** from the server's `lib/` directory are installed for new universes as `commands`, `combat` and `utils`.
- **Custom libraries** can be added or replaced when the universe is created, through the `libs` field of `POST /universe/create` or the `.lua` files in a ZIP upload. See [API](API.md).
- The list lives in the universe config as `lib_hashes`.

Libraries run on every execution, so they should **only define
functions and tables**. Top-level code that creates objects would run
every time.

### `Commands`

The built-in `get`/`take`, `drop` and `inventory` commands call these
functions as the player:

- `Commands.take(player_id, item)`
- `Commands.drop(player_id, item)`
- `Commands.inventory(player_id)`

Each returns `{success, message}`. The message is the player's reply, and
the onlookers' broadcast excludes the player.

`Commands.take` finds the item with `game.present` and refuses objects
that are `fixed` or living (`game.is_a(id, "living")`: NPCs, players and
their subclasses).

`Commands.flee` wraps [`game.flee`](#gamefleeid) (the built-in `flee`
command uses the same engine call). The library also defines
`Commands.give`, `Commands.look`, `Commands.say` and `Commands.use`, but no
player command calls them.

### `Combat`

Thin wrappers over the server's combat engine (the [combat
functions](#combat-engine) and status effects), which is the single source
of combat rules: the same rules as the built-in `attack` command (see
[Built-in combat](#built-in-combat)).

- `Combat.can_attack(attacker_id, target_id)`: `true`, or `false` and the reason.
- `Combat.initiate(attacker_id, target_id)` (and its alias `Combat.attack`): starts a fight, one blow now, rounds after. Returns `{success, message, blow}`; during a fight it switches target (no extra blow).
- `Combat.deal_damage(target_id, amount, damage_type, source_id)`: resistances apply; returns `{success, damage, remaining_hp, killed, message}`.
- `Combat.heal(target_id, amount)`: returns `{success, new_hp, message}`.
- `Combat.apply_effect(target_id, effect, duration, magnitude, damage_type, period)`, `Combat.remove_effect(target_id, effect)`, `Combat.effects(target_id)`: the status effect functions.
- `Combat.state(id)`: `game.combat_state`. `Combat.stop(id)`: `game.stop_combat`. `Combat.flee(id)`: `{success, message, direction}`.
- `Combat.is_dead(id)`: gone, at 0 HP, or out of the world (an NPC waiting to respawn). `Combat.get_stats(id)`: `{hp, max_hp, armor_class, attack_bonus, resistances, effects, state}` with the engine's defaults.
- Constants: `Combat.PVP_POLICIES`, `Combat.DAMAGE_TYPES` (the engine's damage types), `Combat.EFFECTS` (the status effect types).

Each needs the same permission as the `game.*` function it wraps.

### `Utils`

Small helpers:

- `get_prop(id, prop, default)`, `set_prop(id, prop, value)`, `exists(id)`
- `find_by_class(parent, class)`, `living_in(room)`
- `split`, `trim`, `capitalize`, `contains`, `deep_copy`, `random_element`
- `clamp`, `lerp`, `format_number`, `format_duration`

---

## Built-in classes

Objects start with the defaults of their class and its ancestors. The
`create` command doesn't apply class defaults; `game.create_object` does.

| Class | Parent | Default properties |
|---|---|---|
| `thing` | — | `name = ""`, `description = ""` |
| `item` | thing | `weight = 0`, `value = 0`, `fixed = false` |
| `living` | thing | `hp = 100`, `max_hp = 100`, `attack_bonus = 0`, `armor_class = 10`, `in_combat = false` |
| `room` | thing | `exits = {}`, `lighting = "normal"`, `region_id = nil` |
| `region` | thing | `environment_type = "dungeon"`, `danger_level = 1`, `ambient_sounds = {}` |
| `weapon` | item | `damage_dice = "1d6"`, `damage_bonus = 0`, `damage_type = "physical"` |
| `armor` | item | `armor_value = 0`, `slot = "body"` |
| `container` | item | `capacity = 10`, `locked = false` |
| `player` | living | `access_level = "player"` |
| `npc` | living | `aggro = false`, `respawn_time = nil` |

Which properties the server reads:

| Property | Used by |
|---|---|
| `name`, `description` | room display, `present`, verb matching |
| `exits` | movement: maps a direction (`north`, `south`, `east`, `west`, `up`, `down`) to a room ID |
| `fixed` | blocks moving by non-builders, and the `take` command |
| `image_hash` (room) | the room picture. If it is missing and Venice is configured, the server generates one in the background the first time a logged-in player sees the room. |
| `region_id` (room), `environment_type` (region) | style hint for those generated pictures |
| `is_portal` (room) | marks a safe room, where returning players respawn |
| `last_safe_location`, `workroom_id` (player) | where a returning player respawns |
| `hp`, `max_hp`, `armor_class`, `attack_bonus` (npc, player) | built-in combat |
| `xp_value`, `respawn_seconds`, `home_room` (npc); `xp` (player) | death and respawn (see [Built-in combat](#built-in-combat)) |
| `aggro`, `flee_threshold`, `faction`, `wander_seconds` (npc) | NPC behaviour (see [Built-in combat](#built-in-combat)) |
| `slot`, `damage`, `damage_type`, `attack_bonus`, `armor_class`, `equipped_by` (item) | equipment (see [Built-in combat](#built-in-combat)) |

All other properties, including `weight`, `value`, `lighting`,
`capacity` and `locked`, are data for your own scripts.

---

## Built-in combat

The built-in `attack <name>` (or `kill`) command works like this:

- A universe chooses whether there is fighting at all with its `combat` config: `on` (default), `npc_only` (creatures only, no player against player whatever `pvp_policy` says) or `off` (a social universe: no attacks, no aggro, no rounds; status effects still work).
- It targets an object in the room whose class is `npc`, a class derived from `npc` (for example with `game.define_class`), or `monster`, or another player as the universe config's `pvp_policy` allows: `arena` (the default: only in a room with `is_arena = true`), `flagged` (both players typed `pvp on`), `open`, or `disabled`. A fight between players ends as soon as the policy no longer allows it.
- HP lives in the objects' properties: `hp`, `max_hp`, `armor_class` (default 10), `attack_bonus` (default 0) and `resistances` (e.g. `{fire = "resistant"}`, also `immune` and `vulnerable`). An attack reads them and writes the target's new `hp` back, as one atomic change.
- Each attack is a d20 roll plus `attack_bonus` against the target's `armor_class`. A hit deals 1d6 physical damage, doubled on a natural 20 (a critical hit), then the target's resistances. A natural 1 always misses.
- Status effects count: a stunned or paralyzed attacker cannot attack; a blinded attacker or an invisible target is -4 to hit, a stunned or paralyzed target +4; `strengthened`/`weakened` on the attacker and `exposed`/`protected` on the target add or subtract their magnitude from the damage roll.
- A critical hit that does not kill makes the target **bleed**: 2 physical damage per second for 3 seconds.
- **Rounds.** The first blow lands at once; then each fighter strikes again every `combat_round_ms` (universe config, default 3000, 1000 to 10000) until the fight ends, without further commands. The target fights back. A fight ends when either side dies or they are no longer in the same room (walk away to flee); a fighter whose target is gone turns on someone still fighting it. Attacking someone else switches target; retyping `attack` during a fight adds no blow (rounds set the pace). A stunned or paralyzed fighter loses its turn.
- A hit fires `on_damage`. A kill fires `on_death` (after `on_damage`).
- **Fleeing:** the `flee` command (or [`game.flee`](#gamefleeid)) leaves through a random exit and ends your fight; your attackers stop once you are gone. It fails with the universe's `flee_fail_percent` chance (default 0), and never works while stunned.
- **NPC scripts:** an NPC's code may define `ai_combat_tick` (run as its owner on each of its rounds, with `{object_id, room_id, target_id, hp}`) and `ai_idle_tick` (run when a player arrives in its room, `{reason = "arrival", arrived_id}`, or when it would wander, `{reason = "wander"}`). If the handler returns anything but `nil`, it replaces the built-in behaviour for that moment (the blow or flee, the aggro, the wander move); `nil`, no handler or an error falls back to the built-in behaviour.
- **Equipment**: `wield <item>` / `unwield`, `wear <item>` / `remove <item>`, `equipment` (or `eq`). An item names its slot in `slot`: `weapon` (an item with `damage` and no slot is one), `head`, `body`, `hands`, `feet`, `shield`, `neck` or `ring`; one item per slot (if marks say otherwise, only the first item of a slot, by id, counts; a clone of worn gear is not worn). A wielded weapon replaces the bare 1d6 with its `damage` dice (e.g. `"1d8"`, `"2d4+1"`; at most 100 dice of 1 to 1000 sides, modifier within ±1000, else the bare 1d6) of its `damage_type` (resistances apply) and adds its `attack_bonus`; each worn item adds its `armor_class` to the wearer's. Equipped items carry `equipped_by` (the wearer); anything that moves an item away (dropping, giving, death, a script) unequips it, and death policies treat gear like other belongings.
- **NPC behaviour** (NPC properties):
  - `aggro = true`: attacks a player who arrives in its room (walking, `goto`, spawning or moved by Lua), and a player in a room it arrives in, at once (PvP rules don't apply to NPCs).
  - `flee_threshold` (a fraction of `max_hp`, e.g. `0.25`): below it, the NPC runs through a random exit on its turn instead of attacking, ending its fight.
  - `faction` (a name): NPCs of the same faction in the room join a fight against one of their own (when it is attacked).
  - `wander_seconds`: about that often (randomized ±50%) the NPC takes a random exit to a room of the same region (the room's `region_id`, else its parent), never while fighting. Setting it starts the wandering; removing it stops it.
- **Deaths** follow the universe config:
  - `player_death`: `corpse` (default: what the player carries is left in a corpse where they died), `keep` (nothing is lost) or `drop` (on the floor). The player respawns at once, at full HP, in their workroom or the universe's spawn room.
  - `npc_death`: `corpse` (default), `drop` or `destroy` (its belongings are deleted). The NPC leaves the world (`parent_id = nil`, HP restored) and comes back to its `home_room` (default: where it died; if that room is gone, where it died, recorded in `died_in`) after its `respawn_seconds` (default 300; `0` deletes it for good, with everything it still holds, after its `on_death` has run). A player who kills it gains its `xp_value` (default its `max_hp`) in their `xp` property.
  - Corpses decay after `corpse_decay_s` (universe config, default 300), spilling what is left into the room. `get <item> from corpse` (or `get all from corpse`) takes things out. It works on corpses, objects of class `container` (or derived from it) and objects with `open_container = true`, never while their `locked` is true.

The Rust combat engine knows these damage types: `physical`, `slashing`,
`piercing`, `bludgeoning`, `fire`, `cold` (alias `ice`), `lightning`,
`acid`, `poison`, `necrotic`, `radiant` (alias `holy`), `psychic`,
`force` and `thunder`. Lua applies and inspects status effects with
[`game.apply_effect`](#status-effects) and friends; for other combat logic,
use the [`Combat`](#combat) library or your own code.

---

## Not yet supported

These are not built yet. See the [roadmap](../../docs/roadmap.md).

- **Unfired handlers:** `on_create`, `on_destroy`, `on_init`, `on_move` and `on_use` never fire on their own. Call the ones other than `on_create` and `on_destroy` with `game.use_object` (which refuses engine hooks), or start a heartbeat yourself.
- **Missing player commands:** there is no `use` or `give` command. Add verbs with `game.add_action`.
- **Combat gaps:** weapons deal one damage type (no elemental parts yet).
- **Universe config:** keys such as `spawn_room` are not read by the server; use `setportal`.

---

## Lua quick reference

Mudcroft uses standard Lua 5.4. These are the points that most often trip
up newcomers. Each comment shows the value of the expression on its left.

```lua
local items = {"sword", "shield"}    -- arrays start at 1
local first = items[1]               -- "sword"
local count = #items                 -- 2
local npc = {name = "Guard", hp = 50}
local hp = npc.hp                    -- 50, same as npc["hp"]

local msg = "HP: " .. hp             -- concatenation is .., not +
local s = string.format("%s has %d HP", npc.name, hp)

if hp <= 0 then                      -- not-equal is ~=, not !=
    -- ...
elseif hp < 20 then
    -- ...
end

for i = 1, 3 do end                  -- 1, 2, 3
for i, v in ipairs(items) do end     -- arrays, in order
for k, v in pairs(npc) do end        -- tables, any order

local ok, err = pcall(game.update_object, "/d/x", {hp = 1})   -- catch errors
```

- `nil` and `false` are the only false values; `0` and `""` are true.
- Declare variables with `local`. Globals leak into the rest of the script.
- There is no `x++`; write `x = x + 1`.

Further reading: [Lua 5.4 manual](https://www.lua.org/manual/5.4/) ·
[Programming in Lua](https://www.lua.org/pil/) ·
[Learn Lua in Y minutes](https://learnxinyminutes.com/docs/lua/)
