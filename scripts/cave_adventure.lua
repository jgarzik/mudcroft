-- Cave Adventure Test World
-- Run via /universe/{id}/run_script API

-- Create region
local cave = game.create_object("/regions/dark-caves", "region", nil, {
    name = "Dark Caves",
    environment_type = "cave"
})

-- Create rooms
local entrance = game.create_object("/rooms/cave-entrance", "room", nil, {
    name = "Cave Entrance",
    description = "A dark opening in the mountainside. Cold air flows from within. Moss-covered rocks frame the entrance, and you can hear water dripping somewhere in the darkness ahead.",
    region_id = "/regions/dark-caves"
})

local passage = game.create_object("/rooms/narrow-passage", "room", nil, {
    name = "Narrow Passage",
    description = "A tight passage barely wide enough for one person. Water drips from stalactites above, forming small pools on the uneven floor. The walls glisten with moisture.",
    region_id = "/regions/dark-caves"
})

local chamber = game.create_object("/rooms/treasure-chamber", "room", nil, {
    name = "Treasure Chamber",
    description = "A vast underground chamber. Gold coins and gems glitter in the dim light filtering through cracks above. Something large growls in the shadows at the far end.",
    region_id = "/regions/dark-caves"
})

local pool = game.create_object("/rooms/underground-pool", "room", nil, {
    name = "Underground Pool",
    description = "A serene underground pool fed by a small waterfall. Bioluminescent fungi cast an eerie blue glow across the still water. Strange fish dart beneath the surface.",
    region_id = "/regions/dark-caves"
})

-- Connect rooms
game.update_object("/rooms/cave-entrance", {exits = {north = "/rooms/narrow-passage"}})
game.update_object("/rooms/narrow-passage", {exits = {south = "/rooms/cave-entrance", north = "/rooms/treasure-chamber", east = "/rooms/underground-pool"}})
game.update_object("/rooms/treasure-chamber", {exits = {south = "/rooms/narrow-passage"}})
game.update_object("/rooms/underground-pool", {exits = {west = "/rooms/narrow-passage"}})

-- Add monsters
game.create_object("/npcs/giant-bat", "npc", "/rooms/narrow-passage", {
    name = "Giant Bat",
    description = "A bat the size of a dog with razor-sharp fangs and leathery wings that span nearly six feet.",
    hp = 15,
    max_hp = 15,
    attack_bonus = 1,
    armor_class = 12
})

game.create_object("/npcs/cave-troll", "npc", "/rooms/treasure-chamber", {
    name = "Cave Troll",
    description = "A massive troll with mottled grey skin and beady red eyes. It guards the treasure with single-minded fury.",
    hp = 50,
    max_hp = 50,
    attack_bonus = 4,
    armor_class = 14
})

-- Add items in pool area
game.create_object("/items/glowing-mushroom", "item", "/rooms/underground-pool", {
    name = "Glowing Mushroom",
    description = "A softly glowing blue mushroom. It pulses with an inner light.",
    value = 25,
    weight = 0.5
})

-- Add treasure
game.create_object("/items/ancient-gold-crown", "item", "/rooms/treasure-chamber", {
    name = "Ancient Gold Crown",
    description = "A crown of pure gold studded with blood-red rubies. It once belonged to a forgotten king.",
    value = 500,
    weight = 2
})

game.create_object("/items/sapphire-necklace", "item", "/rooms/treasure-chamber", {
    name = "Sapphire Necklace",
    description = "A delicate silver chain holding a sapphire the size of a robin's egg.",
    value = 300,
    weight = 0.5
})

game.create_object("/weapons/troll-slayer", "weapon", "/rooms/treasure-chamber", {
    name = "Troll Slayer Sword",
    description = "A legendary blade that glows with a faint blue light near trolls. Runes of power are etched along the blade.",
    damage_dice = "2d6",
    damage_bonus = 3,
    weight = 4
})

-- Add a weapon at entrance for new players
game.create_object("/weapons/rusty-sword", "weapon", "/rooms/cave-entrance", {
    name = "Rusty Short Sword",
    description = "A battered but serviceable short sword. Better than nothing.",
    damage_dice = "1d6",
    damage_bonus = 0,
    weight = 2
})

return "Cave adventure created! Enter from: /rooms/cave-entrance"
