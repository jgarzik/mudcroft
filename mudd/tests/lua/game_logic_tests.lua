-- game_logic_tests.lua
-- User story validation tests for the Mudcroft game engine
--
-- Run from mudd/:
--     cargo test --release --test lua_game_logic_test
--
-- The Rust runner (tests/lua_game_logic_test.rs) embeds this file with
-- include_str! and runs it through an admin's `eval`, once per SECTION
-- (one eval per section keeps each run well inside the eval budget). It
-- prepends two locals to the chunk:
--
--     local ACCOUNTS = { admin = "<id>", wizard = "<id>", player = "<id>",
--                        builder = "<id>", player_body = "<player object path>",
--                        player2 = "<id>", player2_body = "<path>" }
--     local SECTION = "<section name>"   -- or "list" for the section names
--
-- ACCOUNTS holds real accounts created by the runner: access levels come
-- from the accounts table, so the suite acts "as player/builder" with
-- game.set_actor(<account id>) and as the wizard with the wizard's own id.
-- player_body is the player's persistent player object (/players/p-<id>),
-- which the player account owns.
--
-- `print` is a no-op in the sandbox, so the chunk returns its report as a
-- string: one "PASS|FAIL|SKIP <test>" line per test plus a RESULT line.
--
-- API notes for test authors:
-- * game.create_object(path, class, parent_id, props). Paths are absolute
--   (see src/objects/path.rs); P("/rel") gives a path unique to the
--   running test. Custom properties are passed flat in props and read
--   back under obj.metadata (name/description are flattened to the top).
-- * A denied create returns {error = ...}; denied update/delete/move raise
--   Lua errors.
-- * Object code is stored with game.store_code and must return a table of
--   handlers taking one args table ({object_id, actor_id, verb, target_id,
--   code_hash}). game.use_object maps use/hit/look/init to
--   on_use/on_hit/on_look/on_init; any other verb is the handler name.
-- * Class handlers: a class gets code through a `code_hash` property
--   default; parent(class_name, handler_name, args) calls the parent
--   class's handler.

local Test = {}
Test.passed = 0
Test.failed = 0

function Test.assert(condition, msg)
    if condition then
        Test.passed = Test.passed + 1
    else
        Test.failed = Test.failed + 1
        error("ASSERTION FAILED: " .. msg, 0)
    end
end

function Test.assert_eq(a, b, msg)
    Test.assert(a == b, msg .. " (expected " .. tostring(b) .. ", got " .. tostring(a) .. ")")
end

function Test.assert_gt(a, b, msg)
    Test.assert(a > b, msg .. " (expected > " .. tostring(b) .. ", got " .. tostring(a) .. ")")
end

function Test.assert_error(fn, expected_error, msg)
    local ok, err = pcall(fn)
    Test.assert(not ok, msg .. " (expected error)")
    if expected_error then
        Test.assert(string.find(tostring(err), expected_error, 1, true),
            msg .. " (expected error containing '" .. expected_error .. "', got '" .. tostring(err) .. "')")
    end
end

-- A denied create/clone returns {error = ...} instead of raising
function Test.assert_denied(result, expected_error, msg)
    Test.assert(type(result) == "table" and result.error ~= nil, msg .. " (expected {error=...})")
    Test.assert(string.find(result.error, expected_error, 1, true),
        msg .. " (expected error containing '" .. expected_error .. "', got '" .. tostring(result.error) .. "')")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- HELPERS
-- ═══════════════════════════════════════════════════════════════════════════

-- Path segment of the running test (set by the runner)
local CURRENT = "suite"

-- Object path unique to the running test: P("/town") -> "/d/<test>/town"
local function P(rel)
    return "/d/" .. CURRENT .. rel
end

-- Create an object for test setup; raises if the create was refused
local function create(path, class, parent_id, props)
    local obj = game.create_object(path, class, parent_id, props)
    if obj == nil or obj.error ~= nil then
        error("create_object(" .. path .. ") failed: " .. tostring(obj and obj.error), 2)
    end
    return obj
end

-- Act as an account; raises if game.set_actor refuses
local function act_as(account_id)
    if not game.set_actor(account_id) then
        error("game.set_actor(" .. tostring(account_id) .. ") refused", 2)
    end
end

-- Act as the wizard and reset the player's body. Returns the account ids
-- (wizard/player/builder, for game.set_actor) and the player's body
-- object id (body, for moving and inspecting the player in the world).
local function setup_test_world()
    act_as(ACCOUNTS.wizard)
    -- (last_blow_at: tests strike back to back within one script; real
    -- fights wait a round between a fighter's blows)
    game.update_object(ACCOUNTS.player_body, {
        hp = 50,
        armor_class = 10,
        carry_weight = 0,
        max_carry = 100,
        last_blow_at = 0,
    })
    return {
        wizard = ACCOUNTS.wizard,
        player = ACCOUNTS.player,
        builder = ACCOUNTS.builder,
        body = ACCOUNTS.player_body,
    }
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 1: REGION CREATION
-- Wizard creates a new region with code and metadata
-- ═══════════════════════════════════════════════════════════════════════════

function test_region_creation_basic()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/haunted-forest"), "region", nil, {
        name = "Haunted Forest",
        description = "A dark and mysterious forest filled with ancient trees.",
        environment = "forest",
        ambient_light = "dim",
        ambient_sounds = { "wind", "owls", "rustling" },
        danger_level = 3
    })

    Test.assert(region ~= nil, "Region should be created")
    Test.assert_eq(region.name, "Haunted Forest", "Region name should match")
    Test.assert_eq(region.class, "region", "Region class should be 'region'")
    Test.assert_eq(region.metadata.danger_level, 3, "Metadata should be preserved")
end

function test_region_with_attached_code()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Define code that runs when entities enter the region
    local region_code = [[
        return {
            on_enter = function(args)
                local region = game.get_object(args.object_id)
                local entity = game.get_object(args.actor_id)
                if entity and entity.class == "player" then
                    game.send(entity.id, "A chill runs down your spine as you enter " .. region.name .. ".")
                end
            end,
            on_leave = function(args)
                local entity = game.get_object(args.actor_id)
                if entity and entity.class == "player" then
                    game.send(entity.id, "You feel relieved leaving the darkness behind.")
                end
            end
        }
    ]]

    -- Store code and get hash
    local code_hash = game.store_code(region_code)
    Test.assert(code_hash ~= nil, "Code should be stored")
    Test.assert(#code_hash == 64, "Code hash should be 64 chars (SHA-256)")

    -- Create region with code attached
    local region = create(P("/haunted-forest"), "region", nil, {
        name = "Haunted Forest",
        code_hash = code_hash,
        environment = "forest"
    })

    Test.assert_eq(region.code_hash, code_hash, "Region should have code attached")

    -- Verify code can be retrieved
    local retrieved_code = game.get_code(code_hash)
    Test.assert(string.find(retrieved_code, "on_enter", 1, true), "Code should contain on_enter handler")
end

-- (A region's on_enter on crossing in: a scenario test,
-- test_region_init_and_move_hooks, since move hooks run after a script.)

function test_region_permission_check_wizard_required()
    local actors = setup_test_world()

    -- Player cannot create region
    act_as(actors.player)
    Test.assert_denied(game.create_object(P("/forbidden-p"), "region", nil, { name = "Forbidden Region" }),
        "permission", "Player should not be able to create region")

    -- Builder cannot create region
    act_as(actors.builder)
    Test.assert_denied(game.create_object(P("/forbidden-b"), "region", nil, { name = "Forbidden Region" }),
        "permission", "Builder should not be able to create region")

    -- Wizard can create region
    act_as(actors.wizard)
    local region = game.create_object(P("/allowed"), "region", nil, { name = "Allowed Region" })
    Test.assert(region ~= nil and region.error == nil,
        "Wizard should be able to create region (" .. tostring(region and region.error) .. ")")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 2: FIXED OBJECTS
-- Wizard creates room with immovable objects (chair, table)
-- ═══════════════════════════════════════════════════════════════════════════

function test_room_creation_in_region()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Create region
    local region = create(P("/town-square"), "region", nil, {
        name = "Town Square",
        environment = "urban"
    })

    -- Assign builder to this region (a path grant on the region's path)
    local grant = game.grant_path(actors.builder, region.id, false)
    Test.assert(grant.error == nil, "Wizard should grant the region to the builder (" .. tostring(grant.error) .. ")")

    -- Builder can now create room in assigned region
    act_as(actors.builder)
    local room = game.create_object(P("/town-square/tavern"), "room", region.id, {
        name = "Tavern Common Room",
        description = "A warm and inviting common room with a roaring fireplace.",
        exits = {},
        lighting = "warm"
    })

    Test.assert(room ~= nil and room.error == nil,
        "Builder should create room in assigned region (" .. tostring(room and room.error) .. ")")
    Test.assert_eq(room.parent_id, region.id, "Room should be child of region")
end

function test_fixed_object_creation()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Create region and room
    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room = create(P("/town/tavern"), "room", region.id, { name = "Tavern" })

    -- Create fixed furniture
    local oak_table = create(P("/town/tavern/oak-table"), "item", room.id, {
        name = "oak table",
        description = "A heavy oak table, scarred by years of use.",
        fixed = true,
        weight = 200,
        material = "wood",
        interactions = { "examine", "sit_at", "place_item" }
    })

    local chair = create(P("/town/tavern/chair"), "item", room.id, {
        name = "wooden chair",
        description = "A sturdy wooden chair.",
        fixed = true,
        weight = 15,
        material = "wood",
        interactions = { "examine", "sit" }
    })

    Test.assert(oak_table.metadata.fixed == true, "Table should be fixed")
    Test.assert(chair.metadata.fixed == true, "Chair should be fixed")
    Test.assert_eq(oak_table.parent_id, room.id, "Table should be in room")
end

function test_player_cannot_take_fixed_object()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Setup room with fixed chair
    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room = create(P("/town/tavern"), "room", region.id, { name = "Tavern" })
    local chair = create(P("/town/tavern/chair"), "item", room.id, {
        name = "wooden chair",
        fixed = true,
        weight = 15
    })

    -- Move player to room
    game.move_object(actors.body, room.id)

    -- Player attempts to take chair (mudlib command, lib/commands.lua)
    act_as(actors.player)
    local result = Commands.take(actors.body, "wooden chair")

    Test.assert(result.success == false, "Take should fail for fixed object")
    -- lib/commands.lua refuses fixed items with "You can't take that"
    Test.assert(string.find(result.message, "can't take", 1, true),
        "Error message should refuse the take (got '" .. tostring(result.message) .. "')")

    -- Verify chair is still in room
    local updated_chair = game.get_object(chair.id)
    Test.assert_eq(updated_chair.parent_id, room.id, "Chair should still be in room")
end

function test_player_cannot_move_fixed_object()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Setup
    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room1 = create(P("/town/room1"), "room", region.id, { name = "Room 1" })
    local room2 = create(P("/town/room2"), "room", region.id, { name = "Room 2" })
    local heavy_table = create(P("/town/room1/table"), "item", room1.id, {
        name = "heavy table",
        fixed = true
    })

    -- Player tries to move table
    act_as(actors.player)
    Test.assert_error(function()
        game.move_object(heavy_table.id, room2.id)
    end, "fixed", "Player should not move fixed object")

    -- Verify table is still in room1
    local updated_table = game.get_object(heavy_table.id)
    Test.assert_eq(updated_table.parent_id, room1.id, "Table should still be in room1")
end

function test_wizard_can_move_fixed_object()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Setup
    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room1 = create(P("/town/room1"), "room", region.id, { name = "Room 1" })
    local room2 = create(P("/town/room2"), "room", region.id, { name = "Room 2" })
    local heavy_table = create(P("/town/room1/table"), "item", room1.id, {
        name = "heavy table",
        fixed = true
    })

    -- Wizard can move fixed object
    game.move_object(heavy_table.id, room2.id)

    local updated_table = game.get_object(heavy_table.id)
    Test.assert_eq(updated_table.parent_id, room2.id, "Wizard should move fixed object")
end

function test_fixed_object_can_be_interacted_with()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Create chair with sit handler (verb "sit" dispatches to handler "sit")
    local chair_code = [[
        return {
            sit = function(args)
                local chair = game.get_object(args.object_id)
                local actor = game.get_object(args.actor_id)
                game.update_object(actor.id, { sitting_on = chair.id })
                game.broadcast(actor.parent_id, actor.name .. " sits on the " .. chair.name .. ".")
                return { success = true, message = "You sit down on the chair." }
            end,
            on_use = function(args)
                return { success = false, message = "You can't do that." }
            end
        }
    ]]
    local code_hash = game.store_code(chair_code)

    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room = create(P("/town/tavern"), "room", region.id, { name = "Tavern" })
    local chair = create(P("/town/tavern/chair"), "item", room.id, {
        name = "wooden chair",
        code_hash = code_hash,
        fixed = true,
        interactions = { "examine", "sit" }
    })

    -- Move player to room
    game.move_object(actors.body, room.id)

    -- Player sits on chair (interaction should work even though fixed)
    act_as(actors.player)
    local result = game.use_object(chair.id, actors.body, "sit")

    Test.assert(result ~= nil and result.success == true, "Player should be able to sit on fixed chair")

    local updated_player = game.get_object(actors.body)
    Test.assert_eq(updated_player.metadata.sitting_on, chair.id, "Player should be sitting on chair")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 3: CUSTOM WEAPON WITH ELEMENTAL DAMAGE + SPAWNER
-- Fire sword +1 with daily spawn from magic chest
-- ═══════════════════════════════════════════════════════════════════════════

function test_define_custom_weapon_class()
    act_as(ACCOUNTS.wizard)

    -- Class handlers are stored code attached through a code_hash default
    local handlers = game.store_code([[
        return {
            on_hit = function(args)
                return args.result
            end
        }
    ]])

    -- Engine weapon fields: slot, damage (dice), damage_type; damage_parts
    -- (a list of {dice, type}) adds elements per instance
    game.define_class("fire_sword", {
        parent = "weapon",
        properties = {
            slot = { type = "string", default = "weapon" },
            damage = { type = "string", default = "1d8+1" },
            damage_type = { type = "string", default = "slashing" },
            code_hash = { type = "string", default = handlers }
        }
    })

    local class_info = game.get_class("fire_sword")
    Test.assert(class_info ~= nil, "fire_sword class should be defined")
    Test.assert_eq(class_info.parent, "weapon", "Parent should be weapon")
    -- get_class reports each property as its default value
    Test.assert_eq(class_info.properties.damage, "1d8+1", "Default damage dice")
end

-- A Flamebrand: slashing plus fire, as damage parts
local FLAMEBRAND_PARTS = {
    { dice = "1d8+1", type = "slashing" },
    { dice = "1d6", type = "fire" },
}

function test_create_fire_sword_instance()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room = create(P("/dungeon/treasure-room"), "room", region.id, { name = "Treasure Room" })

    local sword = create(P("/dungeon/treasure-room/flamebrand"), "fire_sword", room.id, {
        name = "Flamebrand +1",
        description = "A sword wreathed in magical flames. The blade glows with inner fire.",
        damage_parts = FLAMEBRAND_PARTS,
        value = 500
    })

    Test.assert(sword ~= nil, "Fire sword should be created")
    Test.assert_eq(sword.class, "fire_sword", "Class should be fire_sword")
    Test.assert_eq(sword.metadata.damage_parts[2].type, "fire", "Second part is fire")
end

-- The player wields a fresh Flamebrand (elemental dice `fire_dice`) in a new
-- room with `target_props`; returns the attack's result
local function swing_flamebrand(slug, fire_dice, target_props)
    local actors = setup_test_world()
    act_as(actors.wizard)
    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/" .. slug), "room", region.id, { name = "Pit" })
    create(P("/arena/" .. slug .. "-sword"), "fire_sword", actors.body, {
        name = "Flamebrand +1",
        damage_parts = { { dice = "1d8+1", type = "slashing" }, { dice = fire_dice, type = "fire" } },
        equipped_by = actors.body,
    })
    -- armor_class 1: every roll but a natural 1 hits
    target_props.armor_class = 1
    local target = create(P("/arena/" .. slug .. "/target"), "npc", room.id, target_props)
    game.move_object(actors.body, room.id)
    act_as(actors.player)
    local result = Combat.attack_extended(actors.body, target.id)
    act_as(ACCOUNTS.admin)
    game.stop_combat(target.id)
    return result, target
end

function test_fire_sword_damage_against_normal_enemy()
    local result, goblin = swing_flamebrand("goblin-pit", "1d6", { name = "Goblin", hp = 300, max_hp = 300 })
    Test.assert(result.success, "attack starts: " .. tostring(result.message))
    if result.hit then
        local updated = game.get_object(goblin.id)
        Test.assert_eq(300 - updated.metadata.hp, result.damage, "HP drops by the total")
        Test.assert_gt(result.physical_damage, 0, "Physical damage should be dealt")
        Test.assert_gt(result.elemental_damage_rolled, 0, "Elemental damage should be rolled")
        Test.assert_eq(result.elemental_damage_applied, result.elemental_damage_rolled, "Full fire damage to normal enemy")
    end
end

function test_fire_sword_against_fire_immune_enemy()
    local result, elemental = swing_flamebrand("fire-pit", "1d6", {
        name = "Fire Elemental", hp = 300, max_hp = 300, resistances = { fire = "immune" },
    })
    Test.assert(result.success, "attack starts: " .. tostring(result.message))
    if result.hit then
        Test.assert_gt(result.physical_damage, 0, "Physical damage should be dealt")
        Test.assert_gt(result.elemental_damage_rolled, 0, "Fire damage was rolled")
        Test.assert_eq(result.elemental_damage_applied, 0, "Fire damage should be ZERO (immune)")
        local taken = 300 - game.get_object(elemental.id).metadata.hp
        Test.assert_eq(taken, result.physical_damage, "Only physical damage should be taken")
        Test.assert(string.find(result.message, "immune", 1, true), "Message should mention immunity")
    end
end

function test_fire_sword_against_fire_resistant_enemy()
    local result = swing_flamebrand("cave", "2d6", {
        name = "Young Red Dragon", hp = 300, max_hp = 300, resistances = { fire = "resistant" },
    })
    Test.assert(result.success, "attack starts: " .. tostring(result.message))
    if result.hit then
        local expected_reduced = math.floor(result.elemental_damage_rolled / 2)
        Test.assert_eq(result.elemental_damage_applied, expected_reduced, "Fire damage should be halved")
        Test.assert(string.find(result.message, "resist", 1, true), "Message should mention resistance")
    end
end

-- Spawner chest: "open" spawns a Flamebrand +1 into the chest's room, at
-- most once per 24 hours (game.time() is deterministic via game.set_time)
local SPAWNER_CHEST_CODE = [[
    return {
        open = function(args)
            local chest = game.get_object(args.object_id)
            local meta = chest.metadata
            local now = game.time()
            local cooldown = 86400000  -- 24 hours in ms

            -- Check cooldown
            if meta.last_spawn and (now - meta.last_spawn) < cooldown then
                local remaining = math.ceil((cooldown - (now - meta.last_spawn)) / 3600000)
                return {
                    success = false,
                    message = "The chest is empty. It will replenish in " .. remaining .. " hours."
                }
            end

            -- Spawn the sword
            local count = (meta.spawn_count or 0) + 1
            local sword = game.create_object(chest.parent_id .. "/flamebrand-" .. count, "fire_sword", chest.parent_id, {
                name = "Flamebrand +1",
                description = "A freshly materialized sword, flames dancing along its blade.",
                damage_parts = { { dice = "1d8+1", type = "slashing" }, { dice = "1d6", type = "fire" } },
            })
            if sword.error then
                return { success = false, message = "spawn failed: " .. sword.error }
            end

            -- Update last spawn time
            game.update_object(chest.id, { last_spawn = now, spawn_count = count })

            game.broadcast(chest.parent_id, "The chest glows with magical energy and a flaming sword appears!")
            return {
                success = true,
                message = "A Flamebrand +1 materializes from the chest!",
                spawned = sword.id
            }
        end,
        on_use = function(args)
            return { success = false, message = "You can only open this chest." }
        end
    }
]]

function test_spawner_chest_creates_sword()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local code_hash = game.store_code(SPAWNER_CHEST_CODE)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room = create(P("/dungeon/treasure-chamber"), "room", region.id, { name = "Treasure Chamber" })

    local chest = create(P("/dungeon/treasure-chamber/chest"), "item", room.id, {
        name = "Magic Chest",
        description = "An ornate chest covered in fire runes.",
        code_hash = code_hash,
        fixed = true,
        spawns_class = "fire_sword",
        spawn_cooldown_ms = 86400000  -- 24 hours
    })

    game.move_object(actors.body, room.id)

    -- Player opens chest
    act_as(actors.player)
    local result = game.use_object(chest.id, actors.body, "open")

    Test.assert(result.success == true, "First open should succeed (" .. tostring(result.message) .. ")")
    Test.assert(result.spawned ~= nil, "Should return spawned item id")

    -- Verify sword exists in room
    local items = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#items, 1, "One fire sword should be in room")
    Test.assert_eq(items[1].name, "Flamebrand +1", "Sword name should match")
end

function test_spawner_chest_respects_cooldown()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local code_hash = game.store_code(SPAWNER_CHEST_CODE)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room = create(P("/dungeon/vault"), "room", region.id, { name = "Vault" })
    local chest = create(P("/dungeon/vault/chest"), "item", room.id, {
        name = "Magic Chest",
        code_hash = code_hash,
        fixed = true
    })

    game.move_object(actors.body, room.id)

    -- Set time to known value (game.set_time is wizard+)
    Test.assert(game.set_time(1000000000000), "Wizard should set the time")

    act_as(actors.player)

    -- First open: should work
    local result1 = game.use_object(chest.id, actors.body, "open")
    Test.assert(result1.success == true, "First open should succeed (" .. tostring(result1.message) .. ")")

    -- Immediate second open: should fail (cooldown)
    local result2 = game.use_object(chest.id, actors.body, "open")
    Test.assert(result2.success == false, "Second immediate open should fail")
    Test.assert(string.find(result2.message, "empty", 1, true), "Should say chest is empty")
end

function test_spawner_only_one_sword_per_day()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local code_hash = game.store_code(SPAWNER_CHEST_CODE)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room = create(P("/dungeon/vault"), "room", region.id, { name = "Vault" })
    local chest = create(P("/dungeon/vault/chest"), "item", room.id, {
        name = "Magic Chest",
        code_hash = code_hash,
        fixed = true
    })

    game.move_object(actors.body, room.id)

    local base_time = 1000000000000
    Test.assert(game.set_time(base_time), "Wizard should set the time")
    act_as(actors.player)

    -- First open
    game.use_object(chest.id, actors.body, "open")

    -- Try 10 more times immediately
    for i = 1, 10 do
        local result = game.use_object(chest.id, actors.body, "open")
        Test.assert(result.success == false, "Repeated open #" .. i .. " should fail")
    end

    -- Count swords in room - should be exactly 1
    local swords = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#swords, 1, "Only one sword should exist")

    -- Advance time by 25 hours (as the wizard)
    act_as(actors.wizard)
    Test.assert(game.set_time(base_time + (25 * 3600 * 1000)), "Wizard should advance the time")
    act_as(actors.player)

    -- Now should work again
    local result = game.use_object(chest.id, actors.body, "open")
    Test.assert(result.success == true, "Open after cooldown should succeed (" .. tostring(result.message) .. ")")

    -- Now 2 swords
    swords = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#swords, 2, "Second sword should exist after cooldown")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- ADDITIONAL COMBAT SYSTEM TESTS
-- The Combat library wraps the server's combat engine (game.damage,
-- game.attack, ...): resistances are "immune" (no damage), "resistant"
-- (half), "vulnerable" (double); HP are whole numbers.
-- ═══════════════════════════════════════════════════════════════════════════

-- Damage actually applied by Combat.deal_damage
local function damage_applied(target_id, amount, damage_type)
    local result = Combat.deal_damage(target_id, amount, damage_type)
    Test.assert(result.success, "deal_damage should find the target: " .. tostring(result.message))
    return result.damage
end

-- End every fight an entity is in (fights started by a test go on in the
-- background otherwise)
local function stop_fights(...)
    act_as(ACCOUNTS.admin)
    for _, id in ipairs({...}) do
        game.stop_combat(id)
    end
end

function test_combat_multiple_damage_types()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/test-arena"), "room", region.id, { name = "Test Arena" })

    local golem = create(P("/arena/test-arena/golem"), "npc", room.id, {
        name = "Iron Golem",
        hp = 500,
        max_hp = 500,
        armor_class = 20,
        resistances = {
            poison = "immune", psychic = "immune",
            physical = "resistant",
            lightning = "vulnerable",
        }
    })

    Test.assert_eq(damage_applied(golem.id, 20, "poison"), 0, "Poison damage should be 0 (immune)")
    Test.assert_eq(damage_applied(golem.id, 20, "physical"), 10, "Physical damage should be halved (10)")
    Test.assert_eq(damage_applied(golem.id, 20, "lightning"), 40, "Lightning damage should be doubled (40)")
    Test.assert_eq(damage_applied(golem.id, 20, "fire"), 20, "Fire damage should be normal (20)")
    Test.assert_eq(game.get_object(golem.id).metadata.hp, 430, "HP is whole numbers: 500 - 70")
end

function test_combat_death_handling()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/death-test"), "room", region.id, { name = "Death Test" })

    local minion = create(P("/arena/death-test/minion"), "npc", room.id, {
        name = "Weak Minion",
        hp = 5,
        max_hp = 5,
        armor_class = 8,
        respawn_seconds = 60,
    })

    local result = Combat.deal_damage(minion.id, 10, "physical")
    Test.assert(result.killed, "Lethal damage kills")

    -- The universe's npc_death policy: out of the world until it respawns
    local updated = game.get_object(minion.id)
    Test.assert(updated.parent_id == nil, "A dead NPC leaves the room")
    Test.assert(Combat.is_dead(minion.id), "Combat.is_dead while waiting to respawn")
    Test.assert_eq(updated.metadata.hp, 5, "HP restored for its respawn")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- COMBAT LOOP TESTS (PvM and PvP)
-- Fights run in rounds on the server (combat_round_ms apart): within one
-- script a test can start a fight and see its state, not watch rounds.
-- ═══════════════════════════════════════════════════════════════════════════

function test_combat_initiation_pvm()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room = create(P("/dungeon/monster-den"), "room", region.id, { name = "Monster Den" })

    local goblin = create(P("/dungeon/monster-den/goblin"), "npc", room.id, {
        name = "Goblin", hp = 200, max_hp = 200, armor_class = 12,
    })

    game.move_object(actors.body, room.id)
    act_as(actors.player)

    local result = Combat.initiate(actors.body, goblin.id)
    Test.assert(result.success, "Combat initiation should succeed: " .. tostring(result.message))

    local mine = Combat.state(actors.body)
    Test.assert(mine.in_combat, "Player should be in combat")
    Test.assert_eq(mine.attacking, goblin.id, "Player should be attacking goblin")

    local theirs = Combat.state(goblin.id)
    Test.assert(theirs.in_combat, "Goblin should be in combat")
    Test.assert_eq(theirs.attacking, actors.body, "Goblin should fight back")
    Test.assert_eq(theirs.attackers[1], actors.body, "Player should be in goblin's attackers")

    -- Retyping attack adds no blow
    local again = Combat.initiate(actors.body, goblin.id)
    Test.assert(not again.success, "No extra blow during a fight")

    stop_fights(goblin.id)
end

-- Set a universe setting (game.update_universe needs admin+)
local function set_universe(config)
    act_as(ACCOUNTS.admin)
    Test.assert(game.update_universe(config), "update_universe should succeed")
end

-- Both players' bodies in `room`, the first acting
local function duel_setup(actors, room_id)
    act_as(actors.wizard)
    game.move_object(actors.body, room_id)
    game.move_object(ACCOUNTS.player2_body, room_id)
    act_as(actors.player)
end

function test_combat_pvp_disabled()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/town"), "region", nil, { name = "Town" })
    local room = create(P("/town/square"), "room", region.id, { name = "Town Square", is_arena = true })

    set_universe({ pvp_policy = Combat.PVP_POLICIES.DISABLED })
    duel_setup(actors, room.id)

    local result = Combat.initiate(actors.body, ACCOUNTS.player2_body)
    Test.assert(not result.success, "PvP should be blocked when disabled, even in an arena")
    Test.assert(not Combat.state(actors.body).in_combat, "Player should not be in combat")

    set_universe({ pvp_policy = Combat.PVP_POLICIES.ARENA })
end

function test_combat_pvp_arena_only()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/town"), "region", nil, { name = "Town" })
    local town_square = create(P("/town/square"), "room", region.id, { name = "Town Square", is_arena = false })
    local arena = create(P("/town/arena"), "room", region.id, { name = "Arena", is_arena = true })

    set_universe({ pvp_policy = Combat.PVP_POLICIES.ARENA })

    duel_setup(actors, town_square.id)
    local result1 = Combat.initiate(actors.body, ACCOUNTS.player2_body)
    Test.assert(not result1.success, "PvP should be blocked outside arena")

    duel_setup(actors, arena.id)
    local result2 = Combat.initiate(actors.body, ACCOUNTS.player2_body)
    Test.assert(result2.success, "PvP should be allowed in arena: " .. tostring(result2.message))

    stop_fights(actors.body)
end

function test_combat_pvp_flagged()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/wilderness"), "region", nil, { name = "Wilderness" })
    local room = create(P("/wilderness/forest"), "room", region.id, { name = "Forest" })

    set_universe({ pvp_policy = Combat.PVP_POLICIES.FLAGGED })
    act_as(actors.wizard)
    game.update_object(actors.body, { pvp = false })
    game.update_object(ACCOUNTS.player2_body, { pvp = false })
    duel_setup(actors, room.id)

    Test.assert(not Combat.initiate(actors.body, ACCOUNTS.player2_body).success,
        "PvP blocked when neither flagged")

    act_as(actors.wizard)
    game.update_object(actors.body, { pvp = true })
    act_as(actors.player)
    Test.assert(not Combat.initiate(actors.body, ACCOUNTS.player2_body).success,
        "PvP blocked when only attacker flagged")

    act_as(actors.wizard)
    game.update_object(ACCOUNTS.player2_body, { pvp = true })
    act_as(actors.player)
    local result3 = Combat.initiate(actors.body, ACCOUNTS.player2_body)
    Test.assert(result3.success, "PvP allowed when both flagged: " .. tostring(result3.message))

    stop_fights(actors.body)
    act_as(actors.wizard)
    game.update_object(actors.body, { pvp = false })
    game.update_object(ACCOUNTS.player2_body, { pvp = false })
    set_universe({ pvp_policy = Combat.PVP_POLICIES.ARENA })
end

function test_combat_off_universe()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/meadow"), "region", nil, { name = "Meadow" })
    local room = create(P("/meadow/glade"), "room", region.id, { name = "Glade" })
    local deer = create(P("/meadow/glade/deer"), "npc", room.id, { name = "Deer", hp = 20 })

    set_universe({ combat = "off" })
    act_as(actors.wizard)
    game.move_object(actors.body, room.id)
    act_as(actors.player)
    local ok, why = Combat.can_attack(actors.body, deer.id)
    Test.assert(not ok, "No fighting in a social universe")
    Test.assert(not Combat.initiate(actors.body, deer.id).success, "initiate refused too")

    set_universe({ combat = "on" })
    act_as(actors.player)
    Test.assert(Combat.can_attack(actors.body, deer.id), "Fighting again once combat is on")
end

function test_combat_rounds_scheduled()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/combat-pit"), "room", region.id, { name = "Combat Pit" })
    local goblin = create(P("/arena/combat-pit/goblin"), "npc", room.id, {
        name = "Goblin", hp = 300, max_hp = 300, armor_class = 10,
    })

    game.move_object(actors.body, room.id)
    act_as(actors.player)
    local before = game.time()
    Test.assert(Combat.initiate(actors.body, goblin.id).success, "fight starts")

    -- The next blows are rounds, scheduled ahead (combat_round_ms)
    local mine = Combat.state(actors.body)
    local theirs = Combat.state(goblin.id)
    Test.assert(mine.next_round_at > before, "Player's next round is scheduled")
    Test.assert(theirs.next_round_at > before, "Goblin's next round is scheduled")

    stop_fights(goblin.id)
    Test.assert(not Combat.state(actors.body).in_combat, "stop_combat ends the fight")
end

function test_combat_stop_when_target_dies()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/combat-pit"), "room", region.id, { name = "Combat Pit" })

    local goblin = create(P("/arena/combat-pit/goblin"), "npc", room.id, {
        name = "Goblin", hp = 200, max_hp = 200, armor_class = 5,
    })

    game.move_object(actors.body, room.id)
    act_as(actors.player)
    Test.assert(Combat.initiate(actors.body, goblin.id).success, "fight starts")

    -- Kill the goblin directly (as the wizard: players cannot modify NPCs)
    act_as(actors.wizard)
    Test.assert(Combat.deal_damage(goblin.id, 1000, "physical").killed, "goblin dies")

    Test.assert(not Combat.state(actors.body).in_combat, "A death ends the fight")
end

function test_combat_flee()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/dungeon"), "region", nil, { name = "Dungeon" })
    local room1 = create(P("/dungeon/monster-den"), "room", region.id, { name = "Monster Den", exits = {} })
    local room2 = create(P("/dungeon/corridor"), "room", region.id, {
        name = "Corridor", exits = { south = room1.id }
    })
    game.update_object(room1.id, { exits = { north = room2.id } })

    local goblin = create(P("/dungeon/monster-den/goblin"), "npc", room1.id, {
        name = "Goblin", hp = 200, max_hp = 200,
    })

    game.move_object(actors.body, room1.id)
    act_as(actors.player)
    Test.assert(Combat.initiate(actors.body, goblin.id).success, "fight starts")

    local result = Commands.flee(actors.body)
    Test.assert(result.success, "Flee succeeds (flee_fail_percent 0): " .. tostring(result.message))
    Test.assert(not Combat.state(actors.body).in_combat, "Player should exit combat on successful flee")
    Test.assert_eq(game.environment(actors.body).id, room2.id, "Fled north")

    stop_fights(goblin.id)
end

function test_status_effects_in_combat()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/arena"), "region", nil, { name = "Arena" })
    local room = create(P("/arena/pit"), "room", region.id, { name = "Pit" })
    local dummy = create(P("/arena/pit/dummy"), "npc", room.id, {
        name = "Training Dummy", hp = 100, armor_class = 10,
    })

    game.move_object(actors.body, room.id)
    Combat.apply_effect(actors.body, Combat.EFFECTS.STUNNED, 30, 1)

    act_as(actors.player)
    local result = Combat.attack(actors.body, dummy.id)
    Test.assert(not result.success, "Attack should fail while stunned")

    -- Effects are rows the server ends on their deadline; removed here
    act_as(actors.wizard)
    local found = false
    for _, e in ipairs(Combat.effects(actors.body)) do
        if e.type == "stunned" then found = true end
    end
    Test.assert(found, "Stunned is listed")
    Test.assert(Combat.remove_effect(actors.body, Combat.EFFECTS.STUNNED), "Stunned removed")
    act_as(actors.player)
    Test.assert(Combat.attack(actors.body, dummy.id).success, "Can attack again")

    stop_fights(dummy.id)
end

-- ═══════════════════════════════════════════════════════════════════════════
-- PERMISSION TESTS
-- ═══════════════════════════════════════════════════════════════════════════

function test_player_cannot_create_region()
    local actors = setup_test_world()
    act_as(actors.player)

    Test.assert_denied(game.create_object(P("/player-region"), "region", nil, { name = "Player Region" }),
        "permission", "Player should not create region")
end

function test_builder_can_create_room_in_assigned_region()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Create region and assign to builder (path grant)
    local region = create(P("/builder-zone"), "region", nil, { name = "Builder Zone" })
    local grant = game.grant_path(actors.builder, region.id, false)
    Test.assert(grant.error == nil, "Wizard should grant the region to the builder (" .. tostring(grant.error) .. ")")

    -- Builder creates room in their region
    act_as(actors.builder)
    local room = game.create_object(P("/builder-zone/room"), "room", region.id, {
        name = "Builder's Room",
        description = "A room created by a builder."
    })

    Test.assert(room ~= nil and room.error == nil,
        "Builder should create room in assigned region (" .. tostring(room and room.error) .. ")")
end

function test_builder_cannot_create_room_in_unassigned_region()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Create region but DON'T assign to builder
    local region = create(P("/restricted-zone"), "region", nil, { name = "Restricted Zone" })

    -- Builder tries to create room in unassigned region
    act_as(actors.builder)
    Test.assert_denied(game.create_object(P("/restricted-zone/room"), "room", region.id, { name = "Forbidden Room" }),
        "permission", "Builder should not create room in unassigned region")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- CLASS INHERITANCE TESTS
-- ═══════════════════════════════════════════════════════════════════════════

function test_class_inheritance_chain()
    -- Define the inheritance chain: thing → item → weapon → sword → fire_sword
    -- (thing, item and weapon are built-in; we define sword and fire_sword)

    game.define_class("sword", {
        parent = "weapon",
        properties = {
            damage_dice = { type = "string", default = "1d8" },  -- Override
            blade_type = { type = "string", default = "long" },  -- New
        }
    })

    game.define_class("fire_sword", {
        parent = "sword",
        properties = {
            damage_bonus = { type = "number", default = 1 },
            elemental_damage_dice = { type = "string", default = "1d6" },
            elemental_damage_type = { type = "string", default = "fire" },
        }
    })

    -- Check inheritance chain
    local chain = game.get_class_chain("fire_sword")
    Test.assert_eq(chain[1], "fire_sword", "Chain starts with fire_sword")
    Test.assert_eq(chain[2], "sword", "Parent is sword")
    Test.assert_eq(chain[3], "weapon", "Grandparent is weapon")
    Test.assert_eq(chain[4], "item", "Great-grandparent is item")
    Test.assert_eq(chain[5], "thing", "Root is thing")
end

function test_property_inheritance()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/test"), "region", nil, { name = "Test" })
    local room = create(P("/test/armory"), "room", region.id, { name = "Armory" })

    -- Create fire_sword with minimal overrides
    local sword = create(P("/test/armory/blade"), "fire_sword", room.id, {
        name = "Test Flame Blade"
        -- Don't specify other properties - they should inherit
    })

    -- Properties from fire_sword
    Test.assert_eq(sword.metadata.damage_bonus, 1, "damage_bonus from fire_sword (1)")
    Test.assert_eq(sword.metadata.elemental_damage_dice, "1d6", "elemental from fire_sword")
    Test.assert_eq(sword.metadata.elemental_damage_type, "fire", "elemental type from fire_sword")

    -- Properties from sword (inherited through fire_sword)
    Test.assert_eq(sword.metadata.damage_dice, "1d8", "damage_dice from sword")
    Test.assert_eq(sword.metadata.blade_type, "long", "blade_type from sword")

    -- Properties from weapon (inherited through sword)
    Test.assert_eq(sword.metadata.damage_type, "physical", "damage_type from weapon")

    -- Properties from item (inherited through weapon)
    Test.assert_eq(sword.metadata.weight, 0, "weight from item")
    Test.assert_eq(sword.metadata.fixed, false, "fixed from item")
end

function test_property_override()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/test"), "region", nil, { name = "Test" })
    local room = create(P("/test/armory"), "room", region.id, { name = "Armory" })

    -- Create fire_sword with property overrides
    local sword = create(P("/test/armory/epic"), "fire_sword", room.id, {
        name = "Epic Flamebrand",
        damage_bonus = 3,      -- Override fire_sword's default of 1
        weight = 5,            -- Override item's default of 1
        damage_dice = "2d6",   -- Override sword's default of 1d8
    })

    -- Overridden properties
    Test.assert_eq(sword.metadata.damage_bonus, 3, "damage_bonus overridden to 3")
    Test.assert_eq(sword.metadata.weight, 5, "weight overridden to 5")
    Test.assert_eq(sword.metadata.damage_dice, "2d6", "damage_dice overridden to 2d6")

    -- Non-overridden properties still inherit
    Test.assert_eq(sword.metadata.elemental_damage_type, "fire", "elemental type still fire")
    Test.assert_eq(sword.metadata.blade_type, "long", "blade_type still long")
end

function test_is_a_query()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local region = create(P("/test"), "region", nil, { name = "Test" })
    local room = create(P("/test/armory"), "room", region.id, { name = "Armory" })

    local sword = create(P("/test/armory/blade"), "fire_sword", room.id, {
        name = "Test Blade"
    })

    -- is_a checks entire inheritance chain
    Test.assert(game.is_a(sword.id, "fire_sword"), "sword is_a fire_sword")
    Test.assert(game.is_a(sword.id, "sword"), "sword is_a sword")
    Test.assert(game.is_a(sword.id, "weapon"), "sword is_a weapon")
    Test.assert(game.is_a(sword.id, "item"), "sword is_a item")
    Test.assert(game.is_a(sword.id, "thing"), "sword is_a thing")

    -- Negative cases
    Test.assert(not game.is_a(sword.id, "armor"), "sword is NOT armor")
    Test.assert(not game.is_a(sword.id, "room"), "sword is NOT room")
    Test.assert(not game.is_a(sword.id, "player"), "sword is NOT player")
end

-- Handlers below run in their own sandbox (game.use_object runs object code
-- as the object's owner), so they record their calls in the result they
-- return: result.log lists handlers in the order they ran.
function test_handler_inheritance_with_parent()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Define classes with handlers that call parent()
    local weapon_code = game.store_code([[
        return {
            on_hit = function(args)
                args.result = args.result or { hit = true, log = {} }
                table.insert(args.result.log, "weapon_on_hit")
                args.result.weapon_processed = true
                return args.result
            end
        }
    ]])
    game.define_class("test_weapon", {
        parent = "item",
        properties = { code_hash = { type = "string", default = weapon_code } }
    })

    local sword_code = game.store_code([[
        return {
            on_hit = function(args)
                args.result = args.result or { hit = true, log = {} }
                -- Call parent first
                local result = parent("test_sword", "on_hit", args)
                table.insert(result.log, "sword_on_hit")
                result.sword_processed = true
                return result
            end
        }
    ]])
    game.define_class("test_sword", {
        parent = "test_weapon",
        properties = { code_hash = { type = "string", default = sword_code } }
    })

    local fire_code = game.store_code([[
        return {
            on_hit = function(args)
                args.result = args.result or { hit = true, log = {} }
                -- Call parent first (sword → weapon)
                local result = parent("test_fire_sword", "on_hit", args)
                table.insert(result.log, "fire_sword_on_hit")
                result.fire_processed = true
                return result
            end
        }
    ]])
    game.define_class("test_fire_sword", {
        parent = "test_sword",
        properties = { code_hash = { type = "string", default = fire_code } }
    })

    local region = create(P("/test"), "region", nil, { name = "Test" })
    local room = create(P("/test/arena"), "room", region.id, { name = "Arena" })
    local sword = create(P("/test/arena/blade"), "test_fire_sword", room.id, {
        name = "Test Blade",
        code_hash = fire_code
    })

    local dummy_defender = create(P("/test/arena/dummy"), "npc", room.id, { name = "Dummy", hp = 10 })

    -- Trigger on_hit (verb "hit")
    local result = game.use_object(sword.id, actors.body, "hit", dummy_defender.id)

    -- Verify call order: weapon → sword → fire_sword
    Test.assert_eq(result.log[1], "weapon_on_hit", "weapon handler called first")
    Test.assert_eq(result.log[2], "sword_on_hit", "sword handler called second")
    Test.assert_eq(result.log[3], "fire_sword_on_hit", "fire_sword handler called third")

    -- Verify all handlers contributed to result
    Test.assert(result.weapon_processed, "weapon modified result")
    Test.assert(result.sword_processed, "sword modified result")
    Test.assert(result.fire_processed, "fire_sword modified result")
end

function test_handler_override_without_parent()
    local actors = setup_test_world()
    act_as(actors.wizard)

    local base_code = game.store_code([[
        return {
            on_use = function(args)
                return { success = true, message = "base", log = { "base_on_use" } }
            end
        }
    ]])
    game.define_class("base_item", {
        parent = "item",
        properties = { code_hash = { type = "string", default = base_code } }
    })

    local override_code = game.store_code([[
        return {
            on_use = function(args)
                -- Deliberately NOT calling parent() - complete override
                return { success = true, message = "override", log = { "override_on_use" } }
            end
        }
    ]])
    game.define_class("override_item", {
        parent = "base_item",
        properties = { code_hash = { type = "string", default = override_code } }
    })

    local region = create(P("/test"), "region", nil, { name = "Test" })
    local room = create(P("/test/room"), "room", region.id, { name = "Room" })
    local item = create(P("/test/room/special"), "override_item", room.id, {
        name = "Special Item",
        code_hash = override_code
    })

    local result = game.use_object(item.id, actors.body, "use")

    -- Only override handler should have run
    Test.assert_eq(#result.log, 1, "Only one handler called")
    Test.assert_eq(result.log[1], "override_on_use", "Override handler called")
    Test.assert_eq(result.message, "override", "Override result returned")
end

function test_deep_inheritance_legendary_sword()
    local actors = setup_test_world()
    act_as(actors.wizard)

    -- Define legendary_fire_sword (extends fire_sword)
    game.define_class("legendary_fire_sword", {
        parent = "fire_sword",
        properties = {
            damage_bonus = { type = "number", default = 3 },
            elemental_damage_dice = { type = "string", default = "2d6" },
            sentient = { type = "boolean", default = true },
            sword_name = { type = "string", default = "Flamebrand" },
        }
    })

    local region = create(P("/vault"), "region", nil, { name = "Vault" })
    local room = create(P("/vault/treasury"), "room", region.id, { name = "Treasury" })

    local legendary = create(P("/vault/treasury/excalibur"), "legendary_fire_sword", room.id, {
        name = "Excalibur of Flames",
        sword_name = "Ignis"  -- Override the default sword_name
    })

    -- Check 5-level inheritance: legendary_fire_sword → fire_sword → sword → weapon → item → thing
    local chain = game.get_class_chain("legendary_fire_sword")
    Test.assert_eq(#chain, 6, "Chain should have 6 levels")

    -- Properties from legendary_fire_sword
    Test.assert_eq(legendary.metadata.damage_bonus, 3, "+3 from legendary")
    Test.assert_eq(legendary.metadata.elemental_damage_dice, "2d6", "2d6 from legendary")
    Test.assert_eq(legendary.metadata.sentient, true, "sentient from legendary")
    Test.assert_eq(legendary.metadata.sword_name, "Ignis", "sword_name overridden")

    -- Properties from fire_sword (not overridden by legendary)
    Test.assert_eq(legendary.metadata.elemental_damage_type, "fire", "fire type inherited")

    -- Properties from sword
    Test.assert_eq(legendary.metadata.blade_type, "long", "blade_type inherited")

    -- is_a checks all levels
    Test.assert(game.is_a(legendary.id, "legendary_fire_sword"), "is legendary_fire_sword")
    Test.assert(game.is_a(legendary.id, "fire_sword"), "is fire_sword")
    Test.assert(game.is_a(legendary.id, "sword"), "is sword")
    Test.assert(game.is_a(legendary.id, "weapon"), "is weapon")
    Test.assert(game.is_a(legendary.id, "item"), "is item")
    Test.assert(game.is_a(legendary.id, "thing"), "is thing")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- RUN ALL TESTS
-- ═══════════════════════════════════════════════════════════════════════════

-- Sections run in this order, one eval each. Later sections rely on classes
-- defined by earlier ones (fire_sword).
local SECTIONS = {
    "regions", "fixed_objects", "weapons_spawner", "combat_damage",
    "combat_loop", "permissions", "class_inheritance",
}

-- { section, name, fn } in run order
local TESTS = {
    -- User Story 1: Region Creation
    { "regions", "test_region_creation_basic", test_region_creation_basic },
    { "regions", "test_region_with_attached_code", test_region_with_attached_code },
    { "regions", "test_region_permission_check_wizard_required", test_region_permission_check_wizard_required },

    -- User Story 2: Fixed Objects
    { "fixed_objects", "test_room_creation_in_region", test_room_creation_in_region },
    { "fixed_objects", "test_fixed_object_creation", test_fixed_object_creation },
    { "fixed_objects", "test_player_cannot_take_fixed_object", test_player_cannot_take_fixed_object },
    { "fixed_objects", "test_player_cannot_move_fixed_object", test_player_cannot_move_fixed_object },
    { "fixed_objects", "test_wizard_can_move_fixed_object", test_wizard_can_move_fixed_object },
    { "fixed_objects", "test_fixed_object_can_be_interacted_with", test_fixed_object_can_be_interacted_with },

    -- User Story 3: Custom Weapon + Spawner
    { "weapons_spawner", "test_define_custom_weapon_class", test_define_custom_weapon_class },
    { "weapons_spawner", "test_create_fire_sword_instance", test_create_fire_sword_instance },
    { "weapons_spawner", "test_fire_sword_damage_against_normal_enemy", test_fire_sword_damage_against_normal_enemy },
    { "weapons_spawner", "test_fire_sword_against_fire_immune_enemy", test_fire_sword_against_fire_immune_enemy },
    { "weapons_spawner", "test_fire_sword_against_fire_resistant_enemy", test_fire_sword_against_fire_resistant_enemy },
    { "weapons_spawner", "test_spawner_chest_creates_sword", test_spawner_chest_creates_sword },
    { "weapons_spawner", "test_spawner_chest_respects_cooldown", test_spawner_chest_respects_cooldown },
    { "weapons_spawner", "test_spawner_only_one_sword_per_day", test_spawner_only_one_sword_per_day },

    -- Combat System (Damage)
    { "combat_damage", "test_combat_multiple_damage_types", test_combat_multiple_damage_types },
    { "combat_damage", "test_combat_death_handling", test_combat_death_handling },

    -- Combat Loop (PvM/PvP)
    { "combat_loop", "test_combat_initiation_pvm", test_combat_initiation_pvm },
    { "combat_loop", "test_combat_pvp_disabled", test_combat_pvp_disabled },
    { "combat_loop", "test_combat_pvp_arena_only", test_combat_pvp_arena_only },
    { "combat_loop", "test_combat_pvp_flagged", test_combat_pvp_flagged },
    { "combat_loop", "test_combat_off_universe", test_combat_off_universe },
    { "combat_loop", "test_combat_rounds_scheduled", test_combat_rounds_scheduled },
    { "combat_loop", "test_combat_stop_when_target_dies", test_combat_stop_when_target_dies },
    { "combat_loop", "test_combat_flee", test_combat_flee },
    { "combat_loop", "test_status_effects_in_combat", test_status_effects_in_combat },

    -- Permissions
    { "permissions", "test_player_cannot_create_region", test_player_cannot_create_region },
    { "permissions", "test_builder_can_create_room_in_assigned_region", test_builder_can_create_room_in_assigned_region },
    { "permissions", "test_builder_cannot_create_room_in_unassigned_region", test_builder_cannot_create_room_in_unassigned_region },

    -- Class Inheritance
    { "class_inheritance", "test_class_inheritance_chain", test_class_inheritance_chain },
    { "class_inheritance", "test_property_inheritance", test_property_inheritance },
    { "class_inheritance", "test_property_override", test_property_override },
    { "class_inheritance", "test_is_a_query", test_is_a_query },
    { "class_inheritance", "test_handler_inheritance_with_parent", test_handler_inheritance_with_parent },
    { "class_inheritance", "test_handler_override_without_parent", test_handler_override_without_parent },
    { "class_inheritance", "test_deep_inheritance_legendary_sword", test_deep_inheritance_legendary_sword },
}

-- Tests not run, with the reason. "BUG:" marks a server bug: the test
-- asserts the correct behavior and should pass once the bug is fixed.
local SKIP = {
}

local function run_all_tests(section)
    local known = {}
    for _, s in ipairs(SECTIONS) do known[s] = true end
    if section == "list" then
        return "LUA-SUITE-SECTIONS " .. table.concat(SECTIONS, ",") .. " total=" .. #TESTS
    end
    if not known[section] then
        error("unknown SECTION: " .. tostring(section))
    end

    local lines = { "LUA-SUITE section=" .. section }
    local passed, failed, skipped = 0, 0, 0
    for _, t in ipairs(TESTS) do
        local test_section, name, test_fn = t[1], t[2], t[3]
        if not known[test_section] then
            error(name .. ": unknown section " .. tostring(test_section))
        end
        if type(test_fn) ~= "function" then
            error(name .. ": test function not defined")
        end
        if test_section == section then
            if SKIP[name] then
                skipped = skipped + 1
                table.insert(lines, "SKIP " .. name .. ": " .. SKIP[name])
            else
                CURRENT = string.gsub(name, "_", "-")
                local ok, err = pcall(test_fn)
                -- Reset per-test state: act as the wizard, real time
                game.set_actor(nil)
                game.set_time(0)
                if ok then
                    passed = passed + 1
                    table.insert(lines, "PASS " .. name)
                else
                    failed = failed + 1
                    table.insert(lines, "FAIL " .. name .. ": " .. tostring(err))
                end
            end
        end
    end

    table.insert(lines, string.format(
        "RESULT passed=%d failed=%d skipped=%d assertions_passed=%d",
        passed, failed, skipped, Test.passed))
    return table.concat(lines, "\n")
end

return run_all_tests(SECTION)
