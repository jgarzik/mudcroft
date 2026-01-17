-- Combat system library for HemiMUD
-- Provides high-level combat functions built on game.* primitives

Combat = {}

-- PvP mode constants
Combat.PVP_MODES = {
    DISABLED = 0,
    ARENA_ONLY = 1,
    FLAGGED = 2,
    OPEN = 3
}

-- Damage type constants
Combat.DAMAGE_TYPES = {
    PHYSICAL = "physical",
    FIRE = "fire",
    ICE = "ice",
    LIGHTNING = "lightning",
    POISON = "poison",
    HOLY = "holy",
    DARK = "dark"
}

-- Effect type constants
Combat.EFFECTS = {
    STUN = "stun",
    POISON = "poison",
    BURN = "burn",
    FREEZE = "freeze",
    BLEED = "bleed",
    REGEN = "regen"
}

-- Initiate combat between attacker and defender
-- Returns {success: bool, message: string}
function Combat.initiate(attacker_id, defender_id)
    -- Get attacker and defender objects
    local attacker = game.get_object(attacker_id)
    local defender = game.get_object(defender_id)

    if not attacker then
        return {success = false, message = "Attacker not found"}
    end
    if not defender then
        return {success = false, message = "Defender not found"}
    end

    -- Check if they're in the same location
    if attacker.parent_id ~= defender.parent_id then
        return {success = false, message = "Target is not here"}
    end

    -- TODO: Check PvP mode and permissions

    return {success = true, message = "Combat initiated"}
end

-- Perform an attack with optional weapon
-- Returns {success: bool, damage: number, message: string}
function Combat.attack(attacker_id, defender_id, weapon_id)
    local attacker = game.get_object(attacker_id)
    local defender = game.get_object(defender_id)

    if not attacker or not defender then
        return {success = false, damage = 0, message = "Invalid combatant"}
    end

    -- Calculate base damage
    local base_damage = attacker.metadata and attacker.metadata.strength or 10

    -- Apply weapon modifier if present
    if weapon_id then
        local weapon = game.get_object(weapon_id)
        if weapon and weapon.metadata and weapon.metadata.damage then
            base_damage = base_damage + weapon.metadata.damage
        end
    end

    -- Roll dice for damage
    local roll = game.roll_dice("1d20")
    local damage = math.floor(base_damage * (roll / 20))

    -- Apply damage to defender
    local result = Combat.deal_damage(defender_id, damage, Combat.DAMAGE_TYPES.PHYSICAL)

    return {
        success = true,
        damage = damage,
        message = string.format("%s attacks %s for %d damage",
            attacker.name or "Attacker",
            defender.name or "Defender",
            damage)
    }
end

-- Deal damage to a target
-- Returns {success: bool, remaining_hp: number}
function Combat.deal_damage(target_id, amount, damage_type)
    local target = game.get_object(target_id)
    if not target then
        return {success = false, remaining_hp = 0}
    end

    -- Get current HP
    local current_hp = target.metadata and target.metadata.hp or 100

    -- Apply damage resistance if applicable
    local resistance = 0
    if target.metadata and target.metadata.resistances then
        resistance = target.metadata.resistances[damage_type] or 0
    end

    local actual_damage = math.max(0, amount * (1 - resistance / 100))
    local new_hp = math.max(0, current_hp - actual_damage)

    -- Update target HP
    game.update_object(target_id, {hp = new_hp})

    return {success = true, remaining_hp = new_hp}
end

-- Heal a target
-- Returns {success: bool, new_hp: number}
function Combat.heal(target_id, amount)
    local target = game.get_object(target_id)
    if not target then
        return {success = false, new_hp = 0}
    end

    local current_hp = target.metadata and target.metadata.hp or 100
    local max_hp = target.metadata and target.metadata.max_hp or 100
    local new_hp = math.min(max_hp, current_hp + amount)

    game.update_object(target_id, {hp = new_hp})

    return {success = true, new_hp = new_hp}
end

-- Apply a status effect to a target
-- Returns {success: bool, message: string}
function Combat.apply_effect(target_id, effect_type, duration, strength)
    local target = game.get_object(target_id)
    if not target then
        return {success = false, message = "Target not found"}
    end

    -- Get current effects or create new table
    local effects = target.metadata and target.metadata.effects or {}

    -- Add or update effect
    effects[effect_type] = {
        duration = duration or 3,
        strength = strength or 1,
        applied_at = game.time()
    }

    game.update_object(target_id, {effects = effects})

    return {success = true, message = "Effect applied"}
end

-- Remove a status effect from a target
function Combat.remove_effect(target_id, effect_type)
    local target = game.get_object(target_id)
    if not target then
        return false
    end

    if target.metadata and target.metadata.effects then
        local effects = target.metadata.effects
        effects[effect_type] = nil
        game.update_object(target_id, {effects = effects})
    end

    return true
end

-- Process status effects for an entity (call on heartbeat)
function Combat.process_effects(entity_id)
    local entity = game.get_object(entity_id)
    if not entity or not entity.metadata or not entity.metadata.effects then
        return
    end

    local current_time = game.time()
    local effects = entity.metadata.effects
    local expired = {}

    for effect_type, effect in pairs(effects) do
        -- Check if effect has expired
        if effect.applied_at and effect.duration then
            local elapsed = (current_time - effect.applied_at) / 1000 -- convert to seconds
            if elapsed >= effect.duration then
                table.insert(expired, effect_type)
            else
                -- Apply effect tick
                if effect_type == Combat.EFFECTS.POISON then
                    Combat.deal_damage(entity_id, effect.strength, Combat.DAMAGE_TYPES.POISON)
                elseif effect_type == Combat.EFFECTS.BURN then
                    Combat.deal_damage(entity_id, effect.strength, Combat.DAMAGE_TYPES.FIRE)
                elseif effect_type == Combat.EFFECTS.REGEN then
                    Combat.heal(entity_id, effect.strength)
                end
            end
        end
    end

    -- Remove expired effects
    for _, effect_type in ipairs(expired) do
        effects[effect_type] = nil
    end

    if #expired > 0 then
        game.update_object(entity_id, {effects = effects})
    end
end

-- Check if an entity is dead
function Combat.is_dead(entity_id)
    local entity = game.get_object(entity_id)
    if not entity then
        return true
    end

    local hp = entity.metadata and entity.metadata.hp or 100
    return hp <= 0
end

-- Get combat stats for an entity
function Combat.get_stats(entity_id)
    local entity = game.get_object(entity_id)
    if not entity then
        return nil
    end

    return {
        hp = entity.metadata and entity.metadata.hp or 100,
        max_hp = entity.metadata and entity.metadata.max_hp or 100,
        strength = entity.metadata and entity.metadata.strength or 10,
        defense = entity.metadata and entity.metadata.defense or 10,
        effects = entity.metadata and entity.metadata.effects or {}
    }
end

return Combat
