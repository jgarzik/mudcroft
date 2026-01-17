-- Commands library for HemiMUD
-- Provides high-level player commands built on game.* primitives

Commands = {}

-- Take an item from the current room into player inventory
-- Returns {success: bool, message: string}
function Commands.take(player_id, item_name)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, message = "Player not found"}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false, message = "You are nowhere"}
    end

    -- Find the item in the room
    local item = game.present(item_name, room_id)
    if not item then
        return {success = false, message = "You don't see that here"}
    end

    -- Check if item is takeable
    if item.metadata and item.metadata.fixed then
        return {success = false, message = "You can't take that"}
    end

    -- Move item to player inventory
    game.move_object(item.id, player_id)

    local name = item.name or item_name
    game.send(player_id, string.format("You take %s.", name))

    -- Broadcast to room
    local player_name = player.name or "Someone"
    game.broadcast(room_id, string.format("%s takes %s.", player_name, name))

    return {success = true, message = string.format("You take %s.", name)}
end

-- Drop an item from player inventory into the current room
-- Returns {success: bool, message: string}
function Commands.drop(player_id, item_name)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, message = "Player not found"}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false, message = "You are nowhere"}
    end

    -- Find the item in player inventory
    local item = game.present(item_name, player_id)
    if not item then
        return {success = false, message = "You don't have that"}
    end

    -- Move item to room
    game.move_object(item.id, room_id)

    local name = item.name or item_name
    game.send(player_id, string.format("You drop %s.", name))

    -- Broadcast to room
    local player_name = player.name or "Someone"
    game.broadcast(room_id, string.format("%s drops %s.", player_name, name))

    return {success = true, message = string.format("You drop %s.", name)}
end

-- Give an item to another player
-- Returns {success: bool, message: string}
function Commands.give(player_id, item_name, target_name)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, message = "Player not found"}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false, message = "You are nowhere"}
    end

    -- Find the item in player inventory
    local item = game.present(item_name, player_id)
    if not item then
        return {success = false, message = "You don't have that"}
    end

    -- Find the target in the room
    local target = game.present(target_name, room_id)
    if not target then
        return {success = false, message = "They're not here"}
    end

    -- Check if target can receive items (is living)
    if not game.is_a(target.id, "living") and not game.is_a(target.id, "player") then
        return {success = false, message = "You can't give things to that"}
    end

    -- Move item to target inventory
    game.move_object(item.id, target.id)

    local item_display = item.name or item_name
    local target_display = target.name or target_name
    local player_name = player.name or "Someone"

    game.send(player_id, string.format("You give %s to %s.", item_display, target_display))
    game.send(target.id, string.format("%s gives you %s.", player_name, item_display))

    return {success = true, message = string.format("You give %s to %s.", item_display, target_display)}
end

-- Look at something in the room or inventory
-- Returns {success: bool, description: string}
function Commands.look(player_id, target_name)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, description = "Player not found"}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false, description = "You are nowhere"}
    end

    -- If no target, look at room
    if not target_name or target_name == "" then
        local room = game.get_object(room_id)
        if not room then
            return {success = false, description = "The void stretches endlessly around you."}
        end

        local desc = room.description or "You see nothing special."
        local exits = room.metadata and room.metadata.exits or {}

        -- Build exit list
        local exit_list = {}
        for dir, _ in pairs(exits) do
            table.insert(exit_list, dir)
        end

        local exit_str = ""
        if #exit_list > 0 then
            table.sort(exit_list)
            exit_str = "\nExits: " .. table.concat(exit_list, ", ")
        end

        -- Get contents
        local contents = game.all_inventory(room_id)
        local content_str = ""
        for _, obj in ipairs(contents) do
            if obj.id ~= player_id then
                local name = obj.name or obj.class
                content_str = content_str .. "\n  " .. name
            end
        end
        if content_str ~= "" then
            content_str = "\nYou see:" .. content_str
        end

        return {
            success = true,
            description = desc .. exit_str .. content_str
        }
    end

    -- Look at specific target
    local target = game.present(target_name, room_id)
    if not target then
        -- Try inventory
        target = game.present(target_name, player_id)
    end

    if not target then
        return {success = false, description = "You don't see that here."}
    end

    local desc = target.description or "You see nothing special about it."
    return {success = true, description = desc}
end

-- Flee from combat
-- Returns {success: bool, message: string}
function Commands.flee(player_id)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, message = "Player not found"}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false, message = "You are nowhere"}
    end

    local room = game.get_object(room_id)
    if not room then
        return {success = false, message = "Cannot flee"}
    end

    -- Find available exits
    local exits = room.metadata and room.metadata.exits or {}
    local exit_list = {}
    for dir, dest in pairs(exits) do
        table.insert(exit_list, {dir = dir, dest = dest})
    end

    if #exit_list == 0 then
        return {success = false, message = "There's nowhere to flee to!"}
    end

    -- Pick random exit
    local idx = math.random(1, #exit_list)
    local escape = exit_list[idx]

    -- Move player
    game.move_object(player_id, escape.dest)

    local player_name = player.name or "Someone"
    game.broadcast(room_id, string.format("%s flees %s!", player_name, escape.dir))
    game.send(player_id, string.format("You flee %s!", escape.dir))

    return {success = true, message = string.format("You flee %s!", escape.dir)}
end

-- Say something to the room
-- Returns {success: bool}
function Commands.say(player_id, message)
    local player = game.get_object(player_id)
    if not player then
        return {success = false}
    end

    local room_id = player.parent_id
    if not room_id then
        return {success = false}
    end

    local player_name = player.name or "Someone"
    game.send(player_id, string.format('You say, "%s"', message))
    game.broadcast(room_id, string.format('%s says, "%s"', player_name, message))

    return {success = true}
end

-- Show player inventory
-- Returns {success: bool, items: table, message: string}
function Commands.inventory(player_id)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, items = {}, message = "Player not found"}
    end

    local contents = game.all_inventory(player_id)
    local items = {}

    for _, obj in ipairs(contents) do
        table.insert(items, {
            id = obj.id,
            name = obj.name or obj.class,
            description = obj.description
        })
    end

    if #items == 0 then
        return {success = true, items = items, message = "You are carrying nothing."}
    end

    local msg = "You are carrying:\n"
    for _, item in ipairs(items) do
        msg = msg .. "  " .. item.name .. "\n"
    end

    return {success = true, items = items, message = msg}
end

-- Use an item
-- Returns {success: bool, message: string}
function Commands.use(player_id, item_name, target_name)
    local player = game.get_object(player_id)
    if not player then
        return {success = false, message = "Player not found"}
    end

    -- Find the item
    local item = game.present(item_name, player_id)
    if not item then
        return {success = false, message = "You don't have that"}
    end

    -- Find target if specified
    local target_id = nil
    if target_name then
        local target = game.present(target_name, player.parent_id)
        if target then
            target_id = target.id
        else
            target = game.present(target_name, player_id)
            if target then
                target_id = target.id
            end
        end
    end

    -- Try to use the object
    if game.use_object then
        local result = game.use_object(item.id, player_id, "use", target_id)
        if result then
            return result
        end
    end

    return {success = false, message = "You can't use that."}
end

return Commands
