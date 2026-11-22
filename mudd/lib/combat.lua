-- Combat library for Mudcroft
--
-- Thin wrappers over the server's combat engine (game.attack,
-- game.damage, game.heal, game.can_attack, game.combat_state, game.flee and
-- the status effect functions). The engine is the single source of combat
-- rules: to-hit against armor class, weapon dice and damage types,
-- resistances ("immune", "resistant", "vulnerable"), rounds, deaths and
-- the universe's settings (combat, pvp_policy, player_death, npc_death,
-- ...). See langref "Built-in combat".

Combat = {}

-- Universe pvp_policy values
Combat.PVP_POLICIES = {
    DISABLED = "disabled",
    ARENA = "arena",
    FLAGGED = "flagged",
    OPEN = "open",
}

-- Damage types the engine knows (aliases: ice = cold, holy = radiant)
Combat.DAMAGE_TYPES = {
    PHYSICAL = "physical", SLASHING = "slashing", PIERCING = "piercing",
    BLUDGEONING = "bludgeoning", FIRE = "fire", COLD = "cold",
    LIGHTNING = "lightning", ACID = "acid", POISON = "poison",
    NECROTIC = "necrotic", RADIANT = "radiant", PSYCHIC = "psychic",
    FORCE = "force", THUNDER = "thunder",
}

-- Status effect types (game.apply_effect)
Combat.EFFECTS = {
    POISONED = "poisoned", STUNNED = "stunned", BLINDED = "blinded",
    BURNING = "burning", FROZEN = "frozen", PARALYZED = "paralyzed",
    SLOWED = "slowed", HASTED = "hasted", STRENGTHENED = "strengthened",
    WEAKENED = "weakened", PROTECTED = "protected", EXPOSED = "exposed",
    INVISIBLE = "invisible", REGENERATING = "regenerating",
    SILENCED = "silenced", BLEEDING = "bleeding",
}

-- Whether attacker may fight target now: true, or false and the reason
function Combat.can_attack(attacker_id, target_id)
    return game.can_attack(attacker_id, target_id)
end

-- Start a fight: one blow now, then rounds both ways (combat_round_ms).
-- During a fight it switches target (no extra blow).
-- Returns {success, message, blow} (blow: {hit, critical, fumble, damage,
-- killed, text, parts})
function Combat.initiate(attacker_id, target_id)
    local blow, why = game.attack(attacker_id, target_id)
    if not blow then
        return {success = false, message = why}
    end
    return {success = true, message = blow.text, blow = blow}
end

-- Same as initiate (the wielded weapon is the attacker's equipment)
function Combat.attack(attacker_id, target_id)
    return Combat.initiate(attacker_id, target_id)
end

-- Damage of a type, after the target's resistances; a death follows the
-- universe's policy. Returns {success, damage, remaining_hp, killed, message}
function Combat.deal_damage(target_id, amount, damage_type, source_id)
    local r, why = game.damage(target_id, amount, damage_type or "physical", source_id)
    if not r then
        return {success = false, damage = 0, remaining_hp = nil, killed = false, message = why}
    end
    return {success = true, damage = r.damage, remaining_hp = r.hp, killed = r.killed}
end

-- Heal up to max HP. Returns {success, new_hp, message}
function Combat.heal(target_id, amount)
    local hp, why = game.heal(target_id, amount)
    return {success = hp ~= nil, new_hp = hp, message = why}
end

-- Status effects (persisted; the server's scheduler runs and ends them)
function Combat.apply_effect(target_id, effect, duration, magnitude, damage_type, period)
    return game.apply_effect(target_id, effect, duration or 3, magnitude or 1, damage_type, period)
end

function Combat.remove_effect(target_id, effect)
    return game.remove_effect(target_id, effect)
end

function Combat.effects(target_id)
    return game.get_effects(target_id)
end

-- End every fight the entity is in (a peace spell, a referee)
function Combat.stop(entity_id)
    return game.stop_combat(entity_id)
end

-- {in_combat, attacking, attackers, next_round_at}
function Combat.state(entity_id)
    return game.combat_state(entity_id)
end

-- Leave through a random exit, ending one's fight. Returns
-- {success, message, direction}
function Combat.flee(entity_id)
    local dir, why = game.flee(entity_id)
    if not dir then
        return {success = false, message = why}
    end
    return {success = true, message = string.format("You flee %s!", dir), direction = dir}
end

-- Gone, at 0 HP, or out of the world (an NPC waiting to respawn)
function Combat.is_dead(entity_id)
    local entity = game.get_object(entity_id)
    if not entity then
        return true
    end
    local hp = entity.metadata and entity.metadata.hp
    if hp ~= nil and hp <= 0 then
        return true
    end
    return entity.parent_id == nil and entity.class ~= "player"
end

-- Combat stats as the engine reads them (defaults: hp/max_hp 100 for
-- players, 10 for others; armor_class 10; attack_bonus 0)
function Combat.get_stats(entity_id)
    local entity = game.get_object(entity_id)
    if not entity then
        return nil
    end
    local m = entity.metadata or {}
    local default_hp = entity.class == "player" and 100 or 10
    return {
        hp = m.hp or default_hp,
        max_hp = m.max_hp or m.hp or default_hp,
        armor_class = m.armor_class or 10,
        attack_bonus = m.attack_bonus or 0,
        resistances = m.resistances or {},
        effects = game.get_effects(entity_id),
        state = game.combat_state(entity_id),
    }
end

return Combat
