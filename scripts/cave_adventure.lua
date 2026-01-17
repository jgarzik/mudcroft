-- Cave Adventure Test World
-- Run via eval command as a wizard

-- Create region
local cave = game.create_object("region", nil, {
    name = "Dark Caves",
    environment_type = "cave"
})

-- Create rooms
local entrance = game.create_object("room", nil, {
    name = "Cave Entrance",
    description = "A dark opening in the mountainside. Cold air flows from within. Moss-covered rocks frame the entrance, and you can hear water dripping somewhere in the darkness ahead.",
    region_id = cave.id
})

local passage = game.create_object("room", nil, {
    name = "Narrow Passage",
    description = "A tight passage barely wide enough for one person. Water drips from stalactites above, forming small pools on the uneven floor. The walls glisten with moisture.",
    region_id = cave.id
})

local chamber = game.create_object("room", nil, {
    name = "Treasure Chamber",
    description = "A vast underground chamber. Gold coins and gems glitter in the dim light filtering through cracks above. Something large growls in the shadows at the far end.",
    region_id = cave.id
})

local pool = game.create_object("room", nil, {
    name = "Underground Pool",
    description = "A serene underground pool fed by a small waterfall. Bioluminescent fungi cast an eerie blue glow across the still water. Strange fish dart beneath the surface.",
    region_id = cave.id
})

-- Connect rooms
game.update_object(entrance.id, {exits = {north = passage.id}})
game.update_object(passage.id, {exits = {south = entrance.id, north = chamber.id, east = pool.id}})
game.update_object(chamber.id, {exits = {south = passage.id}})
game.update_object(pool.id, {exits = {west = passage.id}})

-- Add monsters
game.create_object("npc", passage.id, {
    name = "Giant Bat",
    description = "A bat the size of a dog with razor-sharp fangs and leathery wings that span nearly six feet.",
    hp = 15,
    max_hp = 15,
    attack_bonus = 1,
    armor_class = 12
})

game.create_object("npc", chamber.id, {
    name = "Cave Troll",
    description = "A massive troll with mottled grey skin and beady red eyes. It guards the treasure with single-minded fury.",
    hp = 50,
    max_hp = 50,
    attack_bonus = 4,
    armor_class = 14
})

-- Add items in pool area
game.create_object("item", pool.id, {
    name = "Glowing Mushroom",
    description = "A softly glowing blue mushroom. It pulses with an inner light.",
    value = 25,
    weight = 0.5
})

-- Add treasure
game.create_object("item", chamber.id, {
    name = "Ancient Gold Crown",
    description = "A crown of pure gold studded with blood-red rubies. It once belonged to a forgotten king.",
    value = 500,
    weight = 2
})

game.create_object("item", chamber.id, {
    name = "Sapphire Necklace",
    description = "A delicate silver chain holding a sapphire the size of a robin's egg.",
    value = 300,
    weight = 0.5
})

game.create_object("weapon", chamber.id, {
    name = "Troll Slayer Sword",
    description = "A legendary blade that glows with a faint blue light near trolls. Runes of power are etched along the blade.",
    damage_dice = "2d6",
    damage_bonus = 3,
    weight = 4
})

-- Add a weapon at entrance for new players
game.create_object("weapon", entrance.id, {
    name = "Rusty Short Sword",
    description = "A battered but serviceable short sword. Better than nothing.",
    damage_dice = "1d6",
    damage_bonus = 0,
    weight = 2
})

return "Cave adventure created! Rooms: entrance, passage, chamber, pool. Enter from: " .. entrance.id
