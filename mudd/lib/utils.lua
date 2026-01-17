-- Utility functions for HemiMUD
-- Helper functions and wrappers for common operations

Utils = {}

-- Get the environment (container/room) of an object
-- Wrapper for game.environment() that returns nil safely
function Utils.environment(obj_id)
    if not obj_id then
        return nil
    end
    return game.environment(obj_id)
end

-- Get the parent object's ID of an object
function Utils.parent_id(obj_id)
    if not obj_id then
        return nil
    end
    local obj = game.get_object(obj_id)
    if not obj then
        return nil
    end
    return obj.parent_id
end

-- Check if an object exists
function Utils.exists(obj_id)
    if not obj_id then
        return false
    end
    return game.get_object(obj_id) ~= nil
end

-- Get a property from an object with default value
function Utils.get_prop(obj_id, prop, default)
    local obj = game.get_object(obj_id)
    if not obj then
        return default
    end

    -- Check metadata first
    if obj.metadata and obj.metadata[prop] ~= nil then
        return obj.metadata[prop]
    end

    -- Then check root properties
    if obj[prop] ~= nil then
        return obj[prop]
    end

    return default
end

-- Set a property on an object
function Utils.set_prop(obj_id, prop, value)
    return game.update_object(obj_id, {[prop] = value})
end

-- Find all objects of a class in a container
function Utils.find_by_class(parent_id, class_name)
    local contents = game.get_children(parent_id, {class = class_name})
    return contents or {}
end

-- Get all living entities in a room
function Utils.living_in(room_id)
    return game.get_living_in(room_id) or {}
end

-- Format a number with commas
function Utils.format_number(n)
    local formatted = tostring(n)
    local k
    while true do
        formatted, k = string.gsub(formatted, "^(-?%d+)(%d%d%d)", '%1,%2')
        if k == 0 then break end
    end
    return formatted
end

-- Capitalize first letter of string
function Utils.capitalize(str)
    if not str or str == "" then
        return str
    end
    return str:sub(1, 1):upper() .. str:sub(2)
end

-- Split a string by delimiter
function Utils.split(str, delimiter)
    local result = {}
    local pattern = string.format("([^%s]+)", delimiter)
    for match in string.gmatch(str, pattern) do
        table.insert(result, match)
    end
    return result
end

-- Trim whitespace from string
function Utils.trim(str)
    if not str then
        return ""
    end
    return str:match("^%s*(.-)%s*$")
end

-- Check if table contains value
function Utils.contains(tbl, value)
    for _, v in pairs(tbl) do
        if v == value then
            return true
        end
    end
    return false
end

-- Get table length (works for non-sequence tables too)
function Utils.table_length(tbl)
    local count = 0
    for _ in pairs(tbl) do
        count = count + 1
    end
    return count
end

-- Deep copy a table
function Utils.deep_copy(orig)
    local orig_type = type(orig)
    local copy
    if orig_type == 'table' then
        copy = {}
        for orig_key, orig_value in next, orig, nil do
            copy[Utils.deep_copy(orig_key)] = Utils.deep_copy(orig_value)
        end
        setmetatable(copy, Utils.deep_copy(getmetatable(orig)))
    else
        copy = orig
    end
    return copy
end

-- Get random element from table
function Utils.random_element(tbl)
    if not tbl or #tbl == 0 then
        return nil
    end
    return tbl[math.random(1, #tbl)]
end

-- Clamp a value between min and max
function Utils.clamp(value, min, max)
    return math.max(min, math.min(max, value))
end

-- Linear interpolation
function Utils.lerp(a, b, t)
    return a + (b - a) * t
end

-- Format time duration in human readable format
function Utils.format_duration(ms)
    local seconds = math.floor(ms / 1000)
    if seconds < 60 then
        return seconds .. "s"
    elseif seconds < 3600 then
        local mins = math.floor(seconds / 60)
        local secs = seconds % 60
        return mins .. "m " .. secs .. "s"
    else
        local hours = math.floor(seconds / 3600)
        local mins = math.floor((seconds % 3600) / 60)
        return hours .. "h " .. mins .. "m"
    end
end

return Utils
