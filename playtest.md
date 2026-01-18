# HemiMUD Playtest Guide

## Quick Start

```bash
# 1. Build server
cd mudd && cargo build --release

# 2. Start test environment
scripts/start-testsrv.sh

# 3. Open browser to client (port 3000 or 3001)
# 4. Login: admin / testpass123
# 5. Select "Test Universe"
```

## Loading the Test World

The empty universe needs rooms loaded. Use the `eval` command to load `scripts/cave_adventure.lua`:

```
eval dofile("scripts/cave_adventure.lua")
```

This creates:
- **Rooms**: Cave Entrance, Narrow Passage, Treasure Chamber, Underground Pool
- **Monsters**: Giant Bat (15 HP), Cave Troll (50 HP)
- **Items**: Glowing Mushroom, Ancient Gold Crown, Sapphire Necklace, Troll Slayer Sword, Rusty Short Sword

Then set the portal to the entrance room (use the room ID returned by eval):

```
setportal <entrance-room-id>
```

## Test Scenarios

### 1. Room Navigation
- `look` - View current room description
- `north`, `south`, `east`, `west` - Move between rooms
- Verify room descriptions change
- Verify exits are shown correctly

**Expected path through cave:**
```
Entrance -> (north) -> Passage -> (north) -> Chamber
                    -> (east)  -> Pool
```

### 2. Room Image Generation (Venice AI)
- Requires `VENICE_API_KEY` in `.env`
- Room images should auto-generate on first visit
- Check left panel for room artwork
- Images use Sierra adventure game style prompt

### 3. Object Interaction
- `inventory` or `i` - Check inventory
- `get <item>` - Pick up item
- `drop <item>` - Drop item
- `look <item>` - Examine item

**Test items:**
- Rusty Short Sword (at entrance)
- Glowing Mushroom (at pool)
- Treasure items (at chamber, guarded by troll)

### 4. Combat System
- `attack <target>` or `kill <target>` - Initiate combat
- Combat is turn-based with auto-attack rounds
- HP displayed in player status

**Test encounters:**
- Giant Bat in Narrow Passage (easy, 15 HP)
- Cave Troll in Treasure Chamber (hard, 50 HP)

### 5. Communication
- `say <message>` - Speak in room (visible to other players)

## Verification Checklist

- [ ] Login works
- [ ] Universe selection shows available universes
- [ ] WebSocket connects (status: "connected")
- [ ] `eval` command loads Lua scripts
- [ ] `setportal` sets spawn point
- [ ] `look` shows room description
- [ ] Direction commands move between rooms
- [ ] Room panel updates on movement
- [ ] Room images generate (if Venice configured)
- [ ] `get`/`drop` work for items
- [ ] `attack` initiates combat
- [ ] Combat deals damage and updates HP
- [ ] Killing monster removes it from room

## Troubleshooting

### "Universe not initialized"
Run `setportal <room-id>` after loading the world.

### "You are nowhere"
Portal not set, or room doesn't exist. Load world first with `eval`.

### WebSocket disconnects
Check server logs: `tail -f /tmp/mudcroft-e2e-test/server.log`

### No room images
Verify `VENICE_API_KEY` is set in `.env` file.

### eval command fails
Check Lua syntax. Run smaller commands to debug:
```
eval return game.create_object("room", nil, {name="Test"})
```

## Admin Commands

| Command | Description |
|---------|-------------|
| `eval <lua>` | Execute Lua code (wizard+) |
| `goto <room_id>` | Teleport to room (wizard+) |
| `setportal <room_id>` | Set universe spawn point (wizard+) |

## File Locations

- Server binary: `mudd/target/release/mudd`
- Test world script: `scripts/cave_adventure.lua`
- Server logs: `/tmp/mudcroft-e2e-test/server.log`
- Client logs: `/tmp/mudcroft-e2e-test/client.log`
- Database: `/tmp/mudcroft-e2e-test/mudcroft.db`
