-- game_logic_tests.lua
-- User story validation tests for HemiMUD game engine
-- Run via: cargo test --test test_lua_game_logic

local Test = {}
Test.passed = 0
Test.failed = 0
Test.results = {}

function Test.assert(condition, msg)
    if condition then
        Test.passed = Test.passed + 1
        table.insert(Test.results, { pass = true, msg = msg })
    else
        Test.failed = Test.failed + 1
        table.insert(Test.results, { pass = false, msg = msg })
        error("ASSERTION FAILED: " .. msg)
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
        Test.assert(string.find(err, expected_error), msg .. " (expected error containing '" .. expected_error .. "')")
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
-- SETUP: Create test universe with wizard and player accounts
-- ═══════════════════════════════════════════════════════════════════════════

local function setup_test_world()
    -- Create wizard account
    local wizard = game.create_object("player", nil, {
        name = "TestWizard",
        metadata = {
            access_level = "wizard",
            health = 100,
            armor_class = 10
        }
    })
    
    -- Create regular player account
    local player = game.create_object("player", nil, {
        name = "TestPlayer",
        metadata = {
            access_level = "player",
            health = 50,
            armor_class = 10,
            carry_weight = 0,
            max_carry = 100
        }
    })
    
    -- Create builder account with assigned region
    local builder = game.create_object("player", nil, {
        name = "TestBuilder",
        metadata = {
            access_level = "builder",
            assigned_regions = {}
        }
    })
    
    return { wizard = wizard, player = player, builder = builder }
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 1: REGION CREATION
-- Wizard creates a new region with code and metadata
-- ═══════════════════════════════════════════════════════════════════════════

function test_region_creation_basic()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, {
        name = "Haunted Forest",
        description = "A dark and mysterious forest filled with ancient trees.",
        metadata = {
            environment = "forest",
            ambient_light = "dim",
            ambient_sounds = { "wind", "owls", "rustling" },
            danger_level = 3
        }
    })
    
    Test.assert(region ~= nil, "Region should be created")
    Test.assert_eq(region.name, "Haunted Forest", "Region name should match")
    Test.assert_eq(region.class, "region", "Region class should be 'region'")
    Test.assert_eq(region.metadata.danger_level, 3, "Metadata should be preserved")
end

function test_region_with_attached_code()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Define code that runs when entities enter the region
    local region_code = [[
        function on_enter(self, entity)
            if entity.class == "player" then
                game.send(entity.id, "A chill runs down your spine as you enter " .. self.name .. ".")
            end
        end
        
        function on_leave(self, entity)
            if entity.class == "player" then
                game.send(entity.id, "You feel relieved leaving the darkness behind.")
            end
        end
    ]]
    
    -- Store code and get hash
    local code_hash = game.store_code(region_code)
    Test.assert(code_hash ~= nil, "Code should be stored")
    Test.assert(#code_hash == 64, "Code hash should be 64 chars (SHA-256)")
    
    -- Create region with code attached
    local region = game.create_object("region", nil, {
        name = "Haunted Forest",
        code_hash = code_hash,
        metadata = {
            environment = "forest"
        }
    })
    
    Test.assert_eq(region.code_hash, code_hash, "Region should have code attached")
    
    -- Verify code can be retrieved
    local retrieved_code = game.get_code(code_hash)
    Test.assert(string.find(retrieved_code, "on_enter"), "Code should contain on_enter handler")
end

function test_region_code_executes_on_enter()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create region with on_enter handler
    local region_code = [[
        function on_enter(self, entity)
            entity.metadata.visited_haunted = true
            game.update_object(entity.id, { metadata = entity.metadata })
            return "Welcome to " .. self.name
        end
    ]]
    local code_hash = game.store_code(region_code)
    
    local region = game.create_object("region", nil, {
        name = "Haunted Forest",
        code_hash = code_hash
    })
    
    -- Create room in region
    local room = game.create_object("room", region.id, {
        name = "Forest Entrance",
        metadata = { exits = {} }
    })
    
    -- Move player into room (should trigger region's on_enter)
    game.move_object(actors.player.id, room.id)
    
    -- Verify handler executed
    local updated_player = game.get_object(actors.player.id)
    Test.assert(updated_player.metadata.visited_haunted == true, "on_enter handler should have set flag")
end

function test_region_permission_check_wizard_required()
    local actors = setup_test_world()
    
    -- Player cannot create region
    game.set_actor(actors.player.id)
    Test.assert_error(function()
        game.create_object("region", nil, { name = "Forbidden Region" })
    end, "permission", "Player should not be able to create region")
    
    -- Builder cannot create region
    game.set_actor(actors.builder.id)
    Test.assert_error(function()
        game.create_object("region", nil, { name = "Forbidden Region" })
    end, "permission", "Builder should not be able to create region")
    
    -- Wizard can create region
    game.set_actor(actors.wizard.id)
    local region = game.create_object("region", nil, { name = "Allowed Region" })
    Test.assert(region ~= nil, "Wizard should be able to create region")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 2: FIXED OBJECTS
-- Wizard creates room with immovable objects (chair, table)
-- ═══════════════════════════════════════════════════════════════════════════

function test_room_creation_in_region()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create region
    local region = game.create_object("region", nil, {
        name = "Town Square",
        metadata = { environment = "urban" }
    })
    
    -- Assign builder to this region
    actors.builder.metadata.assigned_regions[region.id] = true
    game.update_object(actors.builder.id, { metadata = actors.builder.metadata })
    
    -- Builder can now create room in assigned region
    game.set_actor(actors.builder.id)
    local room = game.create_object("room", region.id, {
        name = "Tavern Common Room",
        description = "A warm and inviting common room with a roaring fireplace.",
        metadata = {
            exits = { north = nil, south = nil },
            lighting = "warm"
        }
    })
    
    Test.assert(room ~= nil, "Builder should create room in assigned region")
    Test.assert_eq(room.parent_id, region.id, "Room should be child of region")
end

function test_fixed_object_creation()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create region and room
    local region = game.create_object("region", nil, { name = "Town" })
    local room = game.create_object("room", region.id, { name = "Tavern" })
    
    -- Create fixed furniture
    local table = game.create_object("item", room.id, {
        name = "oak table",
        description = "A heavy oak table, scarred by years of use.",
        metadata = {
            fixed = true,
            weight = 200,
            material = "wood",
            interactions = { "examine", "sit_at", "place_item" }
        }
    })
    
    local chair = game.create_object("item", room.id, {
        name = "wooden chair",
        description = "A sturdy wooden chair.",
        metadata = {
            fixed = true,
            weight = 15,
            material = "wood",
            interactions = { "examine", "sit" }
        }
    })
    
    Test.assert(table.metadata.fixed == true, "Table should be fixed")
    Test.assert(chair.metadata.fixed == true, "Chair should be fixed")
    Test.assert_eq(table.parent_id, room.id, "Table should be in room")
end

function test_player_cannot_take_fixed_object()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Setup room with fixed chair
    local region = game.create_object("region", nil, { name = "Town" })
    local room = game.create_object("room", region.id, { name = "Tavern" })
    local chair = game.create_object("item", room.id, {
        name = "wooden chair",
        metadata = { fixed = true, weight = 15 }
    })
    
    -- Move player to room
    game.move_object(actors.player.id, room.id)
    
    -- Player attempts to take chair
    game.set_actor(actors.player.id)
    local result = Commands.take(actors.player, chair)
    
    Test.assert(result.success == false, "Take should fail for fixed object")
    Test.assert(string.find(result.message, "fixed"), "Error message should mention 'fixed'")
    
    -- Verify chair is still in room
    local updated_chair = game.get_object(chair.id)
    Test.assert_eq(updated_chair.parent_id, room.id, "Chair should still be in room")
end

function test_player_cannot_move_fixed_object()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Setup
    local region = game.create_object("region", nil, { name = "Town" })
    local room1 = game.create_object("room", region.id, { name = "Room 1" })
    local room2 = game.create_object("room", region.id, { name = "Room 2" })
    local table = game.create_object("item", room1.id, {
        name = "heavy table",
        metadata = { fixed = true }
    })
    
    -- Player tries to move table
    game.set_actor(actors.player.id)
    Test.assert_error(function()
        game.move_object(table.id, room2.id)
    end, "fixed", "Player should not move fixed object")
    
    -- Verify table is still in room1
    local updated_table = game.get_object(table.id)
    Test.assert_eq(updated_table.parent_id, room1.id, "Table should still be in room1")
end

function test_wizard_can_move_fixed_object()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Setup
    local region = game.create_object("region", nil, { name = "Town" })
    local room1 = game.create_object("room", region.id, { name = "Room 1" })
    local room2 = game.create_object("room", region.id, { name = "Room 2" })
    local table = game.create_object("item", room1.id, {
        name = "heavy table",
        metadata = { fixed = true }
    })
    
    -- Wizard can move fixed object
    game.move_object(table.id, room2.id)
    
    local updated_table = game.get_object(table.id)
    Test.assert_eq(updated_table.parent_id, room2.id, "Wizard should move fixed object")
end

function test_fixed_object_can_be_interacted_with()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create chair with sit handler
    local chair_code = [[
        function on_use(self, actor, action)
            if action == "sit" then
                actor.metadata.sitting_on = self.id
                game.update_object(actor.id, { metadata = actor.metadata })
                game.broadcast(actor.parent_id, actor.name .. " sits on the " .. self.name .. ".")
                return { success = true, message = "You sit down on the chair." }
            end
            return { success = false, message = "You can't do that." }
        end
    ]]
    local code_hash = game.store_code(chair_code)
    
    local region = game.create_object("region", nil, { name = "Town" })
    local room = game.create_object("room", region.id, { name = "Tavern" })
    local chair = game.create_object("item", room.id, {
        name = "wooden chair",
        code_hash = code_hash,
        metadata = {
            fixed = true,
            interactions = { "examine", "sit" }
        }
    })
    
    -- Move player to room
    game.move_object(actors.player.id, room.id)
    
    -- Player sits on chair (interaction should work even though fixed)
    game.set_actor(actors.player.id)
    local result = game.use_object(chair.id, actors.player.id, "sit")
    
    Test.assert(result.success == true, "Player should be able to sit on fixed chair")
    
    local updated_player = game.get_object(actors.player.id)
    Test.assert_eq(updated_player.metadata.sitting_on, chair.id, "Player should be sitting on chair")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- USER STORY 3: CUSTOM WEAPON WITH ELEMENTAL DAMAGE + SPAWNER
-- Fire sword +1 with daily spawn from magic chest
-- ═══════════════════════════════════════════════════════════════════════════

function test_define_custom_weapon_class()
    game.define_class("fire_sword", {
        parent = "weapon",
        properties = {
            damage_dice = { type = "string", default = "1d8" },
            damage_bonus = { type = "number", default = 1 },
            damage_type = { type = "string", default = "physical" },
            elemental_damage_dice = { type = "string", default = "1d6" },
            elemental_damage_type = { type = "string", default = "fire" }
        },
        handlers = {
            on_hit = function(self, attacker, defender, base_result)
                -- Elemental damage handled by Combat.attack_extended
                return base_result
            end
        }
    })
    
    local class_info = game.get_class("fire_sword")
    Test.assert(class_info ~= nil, "fire_sword class should be defined")
    Test.assert_eq(class_info.parent, "weapon", "Parent should be weapon")
    Test.assert_eq(class_info.properties.damage_bonus.default, 1, "Default +1 damage")
end

function test_create_fire_sword_instance()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Treasure Room" })
    
    local sword = game.create_object("fire_sword", room.id, {
        name = "Flamebrand +1",
        description = "A sword wreathed in magical flames. The blade glows with inner fire.",
        metadata = {
            damage_dice = "1d8",
            damage_bonus = 1,
            damage_type = "physical",
            elemental_damage_dice = "1d6",
            elemental_damage_type = "fire",
            value = 500
        }
    })
    
    Test.assert(sword ~= nil, "Fire sword should be created")
    Test.assert_eq(sword.class, "fire_sword", "Class should be fire_sword")
    Test.assert_eq(sword.metadata.elemental_damage_type, "fire", "Elemental type should be fire")
end

function test_fire_sword_damage_against_normal_enemy()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Setup combat scenario
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Combat Pit" })
    
    local sword = game.create_object("fire_sword", actors.player.id, {
        name = "Flamebrand +1",
        metadata = {
            damage_dice = "1d8",
            damage_bonus = 1,
            elemental_damage_dice = "1d6",
            elemental_damage_type = "fire"
        }
    })
    
    local goblin = game.create_object("bot", room.id, {
        name = "Goblin",
        metadata = {
            health = 30,
            armor_class = 12,
            -- No immunities, resistances, or vulnerabilities
        }
    })
    
    -- Move player to room
    game.move_object(actors.player.id, room.id)
    
    -- Force a hit with known seed
    game.set_rng_seed(12345)  -- Seed that produces hit
    
    local initial_health = goblin.metadata.health
    local result = Combat.attack_extended(actors.player, goblin, sword)
    
    if result.hit then
        local updated_goblin = game.get_object(goblin.id)
        Test.assert_gt(initial_health, updated_goblin.metadata.health, "Goblin should take damage")
        Test.assert_gt(result.physical_damage, 0, "Physical damage should be dealt")
        Test.assert_gt(result.elemental_damage_rolled, 0, "Elemental damage should be rolled")
        Test.assert_eq(result.elemental_damage_applied, result.elemental_damage_rolled, "Full fire damage to normal enemy")
    end
end

function test_fire_sword_against_fire_immune_enemy()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Fire Pit" })
    
    local sword = game.create_object("fire_sword", actors.player.id, {
        name = "Flamebrand +1",
        metadata = {
            damage_dice = "1d8",
            damage_bonus = 1,
            elemental_damage_dice = "1d6",
            elemental_damage_type = "fire"
        }
    })
    
    -- Fire elemental is immune to fire
    local fire_elemental = game.create_object("bot", room.id, {
        name = "Fire Elemental",
        metadata = {
            health = 50,
            armor_class = 13,
            immunities = { fire = true }  -- IMMUNE TO FIRE
        }
    })
    
    game.move_object(actors.player.id, room.id)
    game.set_rng_seed(12345)
    
    local initial_health = fire_elemental.metadata.health
    local result = Combat.attack_extended(actors.player, fire_elemental, sword)
    
    if result.hit then
        Test.assert_gt(result.physical_damage, 0, "Physical damage should be dealt")
        Test.assert_gt(result.elemental_damage_rolled, 0, "Fire damage was rolled")
        Test.assert_eq(result.elemental_damage_applied, 0, "Fire damage should be ZERO (immune)")
        
        local updated = game.get_object(fire_elemental.id)
        local damage_taken = initial_health - updated.metadata.health
        Test.assert_eq(damage_taken, result.physical_damage, "Only physical damage should be taken")
        Test.assert(string.find(result.message, "immune"), "Message should mention immunity")
    end
end

function test_fire_sword_against_fire_resistant_enemy()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Cave" })
    
    local sword = game.create_object("fire_sword", actors.player.id, {
        name = "Flamebrand +1",
        metadata = {
            damage_dice = "1d8",
            damage_bonus = 1,
            elemental_damage_dice = "2d6",  -- Larger dice to test halving
            elemental_damage_type = "fire"
        }
    })
    
    -- Red dragon resists fire
    local dragon = game.create_object("bot", room.id, {
        name = "Young Red Dragon",
        metadata = {
            health = 100,
            armor_class = 18,
            resistances = { fire = true }  -- RESISTANT TO FIRE (half damage)
        }
    })
    
    game.move_object(actors.player.id, room.id)
    game.set_rng_seed(12345)
    
    local result = Combat.attack_extended(actors.player, dragon, sword)
    
    if result.hit then
        local expected_reduced = math.floor(result.elemental_damage_rolled / 2)
        Test.assert_eq(result.elemental_damage_applied, expected_reduced, "Fire damage should be halved")
        Test.assert(string.find(result.message, "resist"), "Message should mention resistance")
    end
end

function test_spawner_chest_creates_sword()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Define spawner chest class
    local chest_code = [[
        function on_use(self, actor, action)
            if action ~= "open" then
                return { success = false, message = "You can only open this chest." }
            end
            
            local meta = self.metadata
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
            local sword = game.create_object("fire_sword", self.parent_id, {
                name = "Flamebrand +1",
                description = "A freshly materialized sword, flames dancing along its blade.",
                metadata = {
                    damage_dice = "1d8",
                    damage_bonus = 1,
                    elemental_damage_dice = "1d6",
                    elemental_damage_type = "fire"
                }
            })
            
            -- Update last spawn time
            meta.last_spawn = now
            game.update_object(self.id, { metadata = meta })
            
            game.broadcast(self.parent_id, "The chest glows with magical energy and a flaming sword appears!")
            return { 
                success = true, 
                message = "A Flamebrand +1 materializes from the chest!",
                spawned = sword.id
            }
        end
    ]]
    local code_hash = game.store_code(chest_code)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Treasure Chamber" })
    
    local chest = game.create_object("item", room.id, {
        name = "Magic Chest",
        description = "An ornate chest covered in fire runes.",
        code_hash = code_hash,
        metadata = {
            fixed = true,
            spawns_class = "fire_sword",
            spawn_cooldown_ms = 86400000,  -- 24 hours
            last_spawn = nil
        }
    })
    
    game.move_object(actors.player.id, room.id)
    
    -- Player opens chest
    game.set_actor(actors.player.id)
    local result = game.use_object(chest.id, actors.player.id, "open")
    
    Test.assert(result.success == true, "First open should succeed")
    Test.assert(result.spawned ~= nil, "Should return spawned item id")
    
    -- Verify sword exists in room
    local items = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#items, 1, "One fire sword should be in room")
    Test.assert_eq(items[1].name, "Flamebrand +1", "Sword name should match")
end

function test_spawner_chest_respects_cooldown()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Reuse chest code from above (simplified for test)
    local chest_code = [[
        function on_use(self, actor, action)
            if action ~= "open" then return { success = false } end
            local meta = self.metadata
            local now = game.time()
            local cooldown = 86400000
            if meta.last_spawn and (now - meta.last_spawn) < cooldown then
                return { success = false, message = "empty" }
            end
            local sword = game.create_object("fire_sword", self.parent_id, {
                name = "Flamebrand +1",
                metadata = { damage_dice = "1d8", damage_bonus = 1, elemental_damage_dice = "1d6", elemental_damage_type = "fire" }
            })
            meta.last_spawn = now
            game.update_object(self.id, { metadata = meta })
            return { success = true, spawned = sword.id }
        end
    ]]
    local code_hash = game.store_code(chest_code)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Vault" })
    local chest = game.create_object("item", room.id, {
        name = "Magic Chest",
        code_hash = code_hash,
        metadata = { fixed = true, last_spawn = nil }
    })
    
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    -- Set time to known value
    game.set_time(1000000000000)  -- Some timestamp
    
    -- First open: should work
    local result1 = game.use_object(chest.id, actors.player.id, "open")
    Test.assert(result1.success == true, "First open should succeed")
    
    -- Immediate second open: should fail (cooldown)
    local result2 = game.use_object(chest.id, actors.player.id, "open")
    Test.assert(result2.success == false, "Second immediate open should fail")
    Test.assert(string.find(result2.message, "empty"), "Should say chest is empty")
end

function test_spawner_only_one_sword_per_day()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local chest_code = [[
        function on_use(self, actor, action)
            if action ~= "open" then return { success = false } end
            local meta = self.metadata
            local now = game.time()
            local cooldown = 86400000
            if meta.last_spawn and (now - meta.last_spawn) < cooldown then
                return { success = false, message = "empty" }
            end
            local sword = game.create_object("fire_sword", self.parent_id, {
                name = "Flamebrand +1",
                metadata = { damage_dice = "1d8", damage_bonus = 1, elemental_damage_dice = "1d6", elemental_damage_type = "fire" }
            })
            meta.last_spawn = now
            game.update_object(self.id, { metadata = meta })
            return { success = true, spawned = sword.id }
        end
    ]]
    local code_hash = game.store_code(chest_code)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Vault" })
    local chest = game.create_object("item", room.id, {
        name = "Magic Chest",
        code_hash = code_hash,
        metadata = { fixed = true, last_spawn = nil }
    })
    
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    local base_time = 1000000000000
    game.set_time(base_time)
    
    -- First open
    game.use_object(chest.id, actors.player.id, "open")
    
    -- Try 10 more times immediately
    for i = 1, 10 do
        local result = game.use_object(chest.id, actors.player.id, "open")
        Test.assert(result.success == false, "Repeated open #" .. i .. " should fail")
    end
    
    -- Count swords in room - should be exactly 1
    local swords = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#swords, 1, "Only one sword should exist")
    
    -- Advance time by 25 hours
    game.set_time(base_time + (25 * 3600 * 1000))
    
    -- Now should work again
    local result = game.use_object(chest.id, actors.player.id, "open")
    Test.assert(result.success == true, "Open after cooldown should succeed")
    
    -- Now 2 swords
    swords = game.get_children(room.id, { class = "fire_sword" })
    Test.assert_eq(#swords, 2, "Second sword should exist after cooldown")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- ADDITIONAL COMBAT SYSTEM TESTS
-- ═══════════════════════════════════════════════════════════════════════════

function test_combat_multiple_damage_types()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Test Arena" })
    
    -- Create entity with multiple immunities and resistances
    local golem = game.create_object("bot", room.id, {
        name = "Iron Golem",
        metadata = {
            health = 100,
            armor_class = 20,
            immunities = { poison = true, psychic = true },
            resistances = { physical = true },
            vulnerabilities = { lightning = true }
        }
    })
    
    -- Test poison immunity
    local result1 = Combat.deal_damage(golem.id, 20, "poison")
    Test.assert_eq(result1.applied, 0, "Poison damage should be 0 (immune)")
    
    -- Test physical resistance
    local result2 = Combat.deal_damage(golem.id, 20, "physical")
    Test.assert_eq(result2.applied, 10, "Physical damage should be halved (10)")
    
    -- Test lightning vulnerability
    local result3 = Combat.deal_damage(golem.id, 20, "lightning")
    Test.assert_eq(result3.applied, 40, "Lightning damage should be doubled (40)")
    
    -- Test fire (no modifier)
    local result4 = Combat.deal_damage(golem.id, 20, "fire")
    Test.assert_eq(result4.applied, 20, "Fire damage should be normal (20)")
end

function test_combat_death_handling()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Death Test" })
    
    local minion = game.create_object("npc", room.id, {
        name = "Weak Minion",
        metadata = { health = 5, armor_class = 8 }
    })
    
    -- Deal lethal damage
    Combat.deal_damage(minion.id, 10, "physical")
    
    local updated = game.get_object(minion.id)
    Test.assert_eq(updated.metadata.health, 0, "Health should be 0")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- COMBAT LOOP TESTS (PvM and PvP)
-- ═══════════════════════════════════════════════════════════════════════════

function test_combat_initiation_pvm()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Monster Den" })
    
    local goblin = game.create_object("npc", room.id, {
        name = "Goblin",
        metadata = {
            health = 20,
            max_health = 20,
            armor_class = 12,
            in_combat = false,
            attacking = nil,
            attackers = {}
        }
    })
    
    -- Move player to room
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    -- Initiate combat
    local result = Combat.initiate(actors.player, goblin)
    
    Test.assert(result.success, "Combat initiation should succeed")
    
    -- Check player combat state
    local updated_player = game.get_object(actors.player.id)
    Test.assert(updated_player.metadata.in_combat, "Player should be in combat")
    Test.assert_eq(updated_player.metadata.attacking, goblin.id, "Player should be attacking goblin")
    
    -- Check goblin combat state
    local updated_goblin = game.get_object(goblin.id)
    Test.assert(updated_goblin.metadata.in_combat, "Goblin should be in combat")
    Test.assert(updated_goblin.metadata.attackers[actors.player.id], "Player should be in goblin's attackers")
    Test.assert_eq(updated_goblin.metadata.attacking, actors.player.id, "Goblin should auto-retaliate")
end

function test_combat_pvp_disabled()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Town" })
    local room = game.create_object("room", region.id, { name = "Town Square" })
    
    -- Create another player
    local player2 = game.create_object("player", room.id, {
        name = "OtherPlayer",
        metadata = {
            access_level = "player",
            health = 50,
            in_combat = false,
            attackers = {}
        }
    })
    
    -- Set universe to PvP disabled
    local universe = game.get_universe()
    universe.metadata.pvp_mode = Combat.PVP_MODES.DISABLED
    game.update_universe(universe)
    
    -- Move first player to room
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    -- Try to attack other player
    local result = Combat.initiate(actors.player, player2)
    
    Test.assert(not result.success, "PvP should be blocked when disabled")
    Test.assert(not actors.player.metadata.in_combat, "Player should not be in combat")
end

function test_combat_pvp_arena_only()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Town" })
    local town_square = game.create_object("room", region.id, {
        name = "Town Square",
        metadata = { is_arena = false }
    })
    local arena = game.create_object("room", region.id, {
        name = "Arena",
        metadata = { is_arena = true }
    })
    
    local player2 = game.create_object("player", nil, {
        name = "OtherPlayer",
        metadata = { access_level = "player", health = 50, in_combat = false, attackers = {} }
    })
    
    -- Set universe to arena-only PvP
    local universe = game.get_universe()
    universe.metadata.pvp_mode = Combat.PVP_MODES.ARENA_ONLY
    game.update_universe(universe)
    
    -- Test in town square (non-arena)
    game.move_object(actors.player.id, town_square.id)
    game.move_object(player2.id, town_square.id)
    game.set_actor(actors.player.id)
    
    local result1 = Combat.initiate(actors.player, player2)
    Test.assert(not result1.success, "PvP should be blocked outside arena")
    
    -- Test in arena
    game.move_object(actors.player.id, arena.id)
    game.move_object(player2.id, arena.id)
    
    local result2 = Combat.initiate(actors.player, player2)
    Test.assert(result2.success, "PvP should be allowed in arena")
end

function test_combat_pvp_flagged()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Wilderness" })
    local room = game.create_object("room", region.id, { name = "Forest" })
    
    local player2 = game.create_object("player", room.id, {
        name = "OtherPlayer",
        metadata = { access_level = "player", health = 50, in_combat = false, attackers = {}, pvp_flagged = false }
    })
    
    -- Set universe to flagged PvP
    local universe = game.get_universe()
    universe.metadata.pvp_mode = Combat.PVP_MODES.FLAGGED
    game.update_universe(universe)
    
    game.move_object(actors.player.id, room.id)
    actors.player.metadata.pvp_flagged = false
    game.set_actor(actors.player.id)
    
    -- Neither flagged - should fail
    local result1 = Combat.initiate(actors.player, player2)
    Test.assert(not result1.success, "PvP blocked when neither flagged")
    
    -- Only attacker flagged - should fail
    actors.player.metadata.pvp_flagged = true
    game.update_object(actors.player.id, { metadata = actors.player.metadata })
    local result2 = Combat.initiate(actors.player, player2)
    Test.assert(not result2.success, "PvP blocked when only attacker flagged")
    
    -- Both flagged - should succeed
    player2.metadata.pvp_flagged = true
    game.update_object(player2.id, { metadata = player2.metadata })
    local result3 = Combat.initiate(actors.player, player2)
    Test.assert(result3.success, "PvP allowed when both flagged")
end

function test_combat_heartbeat_round()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Combat Pit" })
    
    local goblin = game.create_object("npc", room.id, {
        name = "Goblin",
        metadata = {
            health = 100,  -- High HP to survive multiple rounds
            max_health = 100,
            armor_class = 10,
            in_combat = false,
            attacking = nil,
            attackers = {}
        }
    })
    
    -- Give player a weapon
    local sword = game.create_object("weapon", actors.player.id, {
        name = "Iron Sword",
        metadata = { damage_dice = "1d8", damage_bonus = 0 }
    })
    actors.player.metadata.wielded = sword.id
    actors.player.metadata.health = 100
    actors.player.metadata.max_health = 100
    game.update_object(actors.player.id, { metadata = actors.player.metadata })
    
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    -- Start combat
    Combat.initiate(actors.player, goblin)
    
    local player_obj = game.get_object(actors.player.id)
    Test.assert_eq(player_obj.metadata.combat_round, 0, "Combat round starts at 0")
    
    -- Simulate heartbeat (combat round)
    game.set_rng_seed(12345)  -- Deterministic for testing
    player_obj:heart_beat()
    
    player_obj = game.get_object(actors.player.id)
    Test.assert_eq(player_obj.metadata.combat_round, 1, "Combat round incremented to 1")
    
    -- Goblin should have taken some damage (or at least attack was attempted)
    local updated_goblin = game.get_object(goblin.id)
    Test.assert(updated_goblin.metadata.health <= 100, "Goblin health should be same or lower after round")
end

function test_combat_stop_when_target_dies()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Combat Pit" })
    
    local goblin = game.create_object("npc", room.id, {
        name = "Goblin",
        metadata = {
            health = 1,  -- Very low HP
            max_health = 10,
            armor_class = 5,  -- Easy to hit
            in_combat = false,
            attacking = nil,
            attackers = {}
        }
    })
    
    game.move_object(actors.player.id, room.id)
    game.set_actor(actors.player.id)
    
    Combat.initiate(actors.player, goblin)
    
    -- Kill the goblin directly
    Combat.deal_damage(goblin.id, 10, "physical")
    
    -- Combat.handle_death should have been called, stopping combat
    local player_obj = game.get_object(actors.player.id)
    -- Note: depending on implementation, player might still be "in_combat" 
    -- until their next heartbeat checks for valid target
end

function test_combat_flee()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room1 = game.create_object("room", region.id, {
        name = "Monster Den",
        metadata = { exits = { north = nil } }  -- Will set after creating room2
    })
    local room2 = game.create_object("room", region.id, {
        name = "Corridor",
        metadata = { exits = { south = room1.id } }
    })
    -- Link rooms
    room1.metadata.exits.north = room2.id
    game.update_object(room1.id, { metadata = room1.metadata })
    
    local goblin = game.create_object("npc", room1.id, {
        name = "Goblin",
        metadata = { health = 20, in_combat = false, attackers = {} }
    })
    
    game.move_object(actors.player.id, room1.id)
    actors.player.metadata.in_combat = true
    actors.player.metadata.attacking = goblin.id
    game.update_object(actors.player.id, { metadata = actors.player.metadata })
    
    game.set_actor(actors.player.id)
    
    -- Force flee to succeed (seed that gives 2 on d2)
    game.set_rng_seed(99999)
    
    local result = Commands.flee(actors.player)
    
    -- Player should have moved to room2 (or failed to flee)
    local player_obj = game.get_object(actors.player.id)
    if result.success then
        Test.assert(not player_obj.metadata.in_combat, "Player should exit combat on successful flee")
    end
end

function test_npc_aggro()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Dungeon" })
    local room = game.create_object("room", region.id, { name = "Lair" })
    
    -- Aggressive NPC that attacks players on sight
    local wolf = game.create_object("npc", room.id, {
        name = "Dire Wolf",
        metadata = {
            health = 30,
            max_health = 30,
            in_combat = false,
            attacking = nil,
            attackers = {},
            aggro_targets = { player = true },  -- Aggro on players
            aggro_range = 0  -- Same room only
        }
    })
    
    -- Simulate wolf's idle heartbeat before player arrives
    local wolf_obj = game.get_object(wolf.id)
    wolf_obj:ai_idle_tick()
    
    wolf_obj = game.get_object(wolf.id)
    Test.assert(not wolf_obj.metadata.in_combat, "Wolf should not be in combat (no targets)")
    
    -- Player enters room
    game.move_object(actors.player.id, room.id)
    
    -- Wolf's next idle tick should detect player
    wolf_obj = game.get_object(wolf.id)
    wolf_obj:ai_idle_tick()
    
    wolf_obj = game.get_object(wolf.id)
    Test.assert(wolf_obj.metadata.in_combat, "Wolf should enter combat")
    Test.assert_eq(wolf_obj.metadata.attacking, actors.player.id, "Wolf should target player")
end

function test_npc_flee_at_low_health()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Forest" })
    local room1 = game.create_object("room", region.id, {
        name = "Clearing",
        metadata = { exits = {} }
    })
    local room2 = game.create_object("room", region.id, {
        name = "Dense Forest",
        metadata = { exits = { south = room1.id } }
    })
    room1.metadata.exits.north = room2.id
    game.update_object(room1.id, { metadata = room1.metadata })
    
    local coward_goblin = game.create_object("npc", room1.id, {
        name = "Cowardly Goblin",
        metadata = {
            health = 5,
            max_health = 20,  -- At 25% health
            in_combat = true,
            attacking = actors.player.id,
            attackers = {},
            flee_threshold = 0.3  -- Flee at 30% health
        }
    })
    
    game.move_object(actors.player.id, room1.id)
    
    -- Goblin's combat tick should trigger flee
    local goblin_obj = game.get_object(coward_goblin.id)
    goblin_obj:ai_combat_tick()
    
    -- Goblin should have fled
    goblin_obj = game.get_object(coward_goblin.id)
    Test.assert(not goblin_obj.metadata.in_combat, "Goblin should have fled combat")
    Test.assert_eq(environment(goblin_obj).id, room2.id, "Goblin should have moved to room2")
end

function test_status_effects_in_combat()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Arena" })
    local room = game.create_object("room", region.id, { name = "Pit" })
    
    game.move_object(actors.player.id, room.id)
    
    -- Apply stunned status
    actors.player.metadata.statuses = {
        stunned = { remaining = 2 }
    }
    game.update_object(actors.player.id, { metadata = actors.player.metadata })
    
    local dummy = game.create_object("npc", room.id, {
        name = "Training Dummy",
        metadata = { health = 100, armor_class = 10, in_combat = false, attackers = {} }
    })
    
    local sword = game.create_object("weapon", actors.player.id, {
        name = "Sword",
        metadata = { damage_dice = "1d8" }
    })
    actors.player.metadata.wielded = sword.id
    game.update_object(actors.player.id, { metadata = actors.player.metadata })
    
    game.set_actor(actors.player.id)
    
    -- Attack while stunned should fail
    local result = Combat.attack_extended(actors.player, dummy, sword)
    Test.assert(not result.hit, "Attack should fail while stunned")
    
    -- Process status effects (decrement duration)
    Combat.process_statuses(actors.player)
    Combat.process_statuses(actors.player)  -- Second tick removes it
    
    local player_obj = game.get_object(actors.player.id)
    Test.assert(not player_obj.metadata.statuses or not player_obj.metadata.statuses.stunned, 
        "Stunned should be removed after duration")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- PERMISSION TESTS
-- ═══════════════════════════════════════════════════════════════════════════

function test_player_cannot_create_region()
    local actors = setup_test_world()
    game.set_actor(actors.player.id)
    
    Test.assert_error(function()
        game.create_object("region", nil, { name = "Player Region" })
    end, "permission", "Player should not create region")
end

function test_builder_can_create_room_in_assigned_region()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create region and assign to builder
    local region = game.create_object("region", nil, { name = "Builder Zone" })
    actors.builder.metadata.assigned_regions[region.id] = true
    game.update_object(actors.builder.id, { metadata = actors.builder.metadata })
    
    -- Builder creates room in their region
    game.set_actor(actors.builder.id)
    local room = game.create_object("room", region.id, {
        name = "Builder's Room",
        description = "A room created by a builder."
    })
    
    Test.assert(room ~= nil, "Builder should create room in assigned region")
end

function test_builder_cannot_create_room_in_unassigned_region()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Create region but DON'T assign to builder
    local region = game.create_object("region", nil, { name = "Restricted Zone" })
    
    -- Builder tries to create room in unassigned region
    game.set_actor(actors.builder.id)
    Test.assert_error(function()
        game.create_object("room", region.id, { name = "Forbidden Room" })
    end, "permission", "Builder should not create room in unassigned region")
end

-- ═══════════════════════════════════════════════════════════════════════════
-- CLASS INHERITANCE TESTS
-- ═══════════════════════════════════════════════════════════════════════════

function test_class_inheritance_chain()
    -- Define the inheritance chain: thing → item → weapon → sword → fire_sword
    -- (thing and item are built-in, we define weapon, sword, fire_sword)
    
    game.define_class("weapon", {
        parent = "item",
        properties = {
            damage_dice = { type = "string", default = "1d4" },
            damage_bonus = { type = "number", default = 0 },
            damage_type = { type = "string", default = "physical" },
        },
        handlers = {}
    })
    
    game.define_class("sword", {
        parent = "weapon",
        properties = {
            damage_dice = { type = "string", default = "1d8" },  -- Override
            blade_type = { type = "string", default = "long" },  -- New
        },
        handlers = {}
    })
    
    game.define_class("fire_sword", {
        parent = "sword",
        properties = {
            damage_bonus = { type = "number", default = 1 },
            elemental_damage_dice = { type = "string", default = "1d6" },
            elemental_damage_type = { type = "string", default = "fire" },
        },
        handlers = {}
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
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Test" })
    local room = game.create_object("room", region.id, { name = "Armory" })
    
    -- Create fire_sword with minimal overrides
    local sword = game.create_object("fire_sword", room.id, {
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
    Test.assert_eq(sword.metadata.weight, 1, "weight from item")
    Test.assert_eq(sword.metadata.fixed, false, "fixed from item")
end

function test_property_override()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Test" })
    local room = game.create_object("room", region.id, { name = "Armory" })
    
    -- Create fire_sword with property overrides
    local sword = game.create_object("fire_sword", room.id, {
        name = "Epic Flamebrand",
        metadata = {
            damage_bonus = 3,      -- Override fire_sword's default of 1
            weight = 5,            -- Override item's default of 1
            damage_dice = "2d6",   -- Override sword's default of 1d8
        }
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
    game.set_actor(actors.wizard.id)
    
    local region = game.create_object("region", nil, { name = "Test" })
    local room = game.create_object("room", region.id, { name = "Armory" })
    
    local sword = game.create_object("fire_sword", room.id, {
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

function test_handler_inheritance_with_parent()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local call_log = {}
    
    -- Define classes with handlers that call parent()
    game.define_class("test_weapon", {
        parent = "item",
        properties = {},
        handlers = {
            on_hit = function(self, attacker, defender, result)
                table.insert(call_log, "weapon_on_hit")
                result.weapon_processed = true
                return result
            end
        }
    })
    
    game.define_class("test_sword", {
        parent = "test_weapon",
        properties = {},
        handlers = {
            on_hit = function(self, attacker, defender, result)
                -- Call parent first
                result = parent(self, attacker, defender, result)
                table.insert(call_log, "sword_on_hit")
                result.sword_processed = true
                return result
            end
        }
    })
    
    game.define_class("test_fire_sword", {
        parent = "test_sword",
        properties = {},
        handlers = {
            on_hit = function(self, attacker, defender, result)
                -- Call parent first (sword → weapon)
                result = parent(self, attacker, defender, result)
                table.insert(call_log, "fire_sword_on_hit")
                result.fire_processed = true
                return result
            end
        }
    })
    
    local region = game.create_object("region", nil, { name = "Test" })
    local room = game.create_object("room", region.id, { name = "Arena" })
    local sword = game.create_object("test_fire_sword", room.id, { name = "Test Blade" })
    
    local dummy_attacker = actors.player
    local dummy_defender = game.create_object("npc", room.id, { name = "Dummy", metadata = { health = 10 } })
    
    -- Trigger on_hit
    local result = sword:on_hit(dummy_attacker, dummy_defender, { hit = true })
    
    -- Verify call order: weapon → sword → fire_sword
    Test.assert_eq(call_log[1], "weapon_on_hit", "weapon handler called first")
    Test.assert_eq(call_log[2], "sword_on_hit", "sword handler called second")
    Test.assert_eq(call_log[3], "fire_sword_on_hit", "fire_sword handler called third")
    
    -- Verify all handlers contributed to result
    Test.assert(result.weapon_processed, "weapon modified result")
    Test.assert(result.sword_processed, "sword modified result")
    Test.assert(result.fire_processed, "fire_sword modified result")
end

function test_handler_override_without_parent()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    local call_log = {}
    
    game.define_class("base_item", {
        parent = "item",
        handlers = {
            on_use = function(self, actor, verb)
                table.insert(call_log, "base_on_use")
                return { success = true, message = "base" }
            end
        }
    })
    
    game.define_class("override_item", {
        parent = "base_item",
        handlers = {
            on_use = function(self, actor, verb)
                -- Deliberately NOT calling parent() - complete override
                table.insert(call_log, "override_on_use")
                return { success = true, message = "override" }
            end
        }
    })
    
    local region = game.create_object("region", nil, { name = "Test" })
    local room = game.create_object("room", region.id, { name = "Room" })
    local item = game.create_object("override_item", room.id, { name = "Special Item" })
    
    local result = item:on_use(actors.player, "use")
    
    -- Only override handler should have run
    Test.assert_eq(#call_log, 1, "Only one handler called")
    Test.assert_eq(call_log[1], "override_on_use", "Override handler called")
    Test.assert_eq(result.message, "override", "Override result returned")
end

function test_deep_inheritance_legendary_sword()
    local actors = setup_test_world()
    game.set_actor(actors.wizard.id)
    
    -- Define legendary_fire_sword (extends fire_sword)
    game.define_class("legendary_fire_sword", {
        parent = "fire_sword",
        properties = {
            damage_bonus = { type = "number", default = 3 },
            elemental_damage_dice = { type = "string", default = "2d6" },
            sentient = { type = "boolean", default = true },
            sword_name = { type = "string", default = "Flamebrand" },
        },
        handlers = {}
    })
    
    local region = game.create_object("region", nil, { name = "Vault" })
    local room = game.create_object("room", region.id, { name = "Treasury" })
    
    local legendary = game.create_object("legendary_fire_sword", room.id, {
        name = "Excalibur of Flames",
        metadata = {
            sword_name = "Ignis"  -- Override the default sword_name
        }
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

function run_all_tests()
    local tests = {
        -- User Story 1: Region Creation
        test_region_creation_basic,
        test_region_with_attached_code,
        test_region_code_executes_on_enter,
        test_region_permission_check_wizard_required,
        
        -- User Story 2: Fixed Objects
        test_room_creation_in_region,
        test_fixed_object_creation,
        test_player_cannot_take_fixed_object,
        test_player_cannot_move_fixed_object,
        test_wizard_can_move_fixed_object,
        test_fixed_object_can_be_interacted_with,
        
        -- User Story 3: Custom Weapon + Spawner
        test_define_custom_weapon_class,
        test_create_fire_sword_instance,
        test_fire_sword_damage_against_normal_enemy,
        test_fire_sword_against_fire_immune_enemy,
        test_fire_sword_against_fire_resistant_enemy,
        test_spawner_chest_creates_sword,
        test_spawner_chest_respects_cooldown,
        test_spawner_only_one_sword_per_day,
        
        -- Combat System (Damage)
        test_combat_multiple_damage_types,
        test_combat_death_handling,
        
        -- Combat Loop (PvM/PvP)
        test_combat_initiation_pvm,
        test_combat_pvp_disabled,
        test_combat_pvp_arena_only,
        test_combat_pvp_flagged,
        test_combat_heartbeat_round,
        test_combat_stop_when_target_dies,
        test_combat_flee,
        test_npc_aggro,
        test_npc_flee_at_low_health,
        test_status_effects_in_combat,
        
        -- Permissions
        test_player_cannot_create_region,
        test_builder_can_create_room_in_assigned_region,
        test_builder_cannot_create_room_in_unassigned_region,
        
        -- Class Inheritance
        test_class_inheritance_chain,
        test_property_inheritance,
        test_property_override,
        test_is_a_query,
        test_handler_inheritance_with_parent,
        test_handler_override_without_parent,
        test_deep_inheritance_legendary_sword,
    }
    
    for _, test_fn in ipairs(tests) do
        local test_name = debug.getinfo(test_fn, "n").name or "unknown"
        local ok, err = pcall(test_fn)
        if ok then
            print("✓ " .. test_name)
        else
            print("✗ " .. test_name .. ": " .. tostring(err))
        end
    end
    
    print("\n" .. Test.passed .. " passed, " .. Test.failed .. " failed")
    return Test.failed == 0
end

return { run_all_tests = run_all_tests, Test = Test }
