# Mudcroft Playtest Guide

How to start a local server with a small world loaded and play it in the
browser. For automated browser tests, see [Automated tests](#automated-tests).

## Prerequisites

- A Rust toolchain, plus Node.js 22 or newer with npm. The start script's
  `setportal` step uses Node's built-in `WebSocket`.
- `curl` and `jq`
- The built server and client:

  ```bash
  (cd mudd && cargo build --release)
  (cd client && npm ci && npm run build)
  ```

## Start

From the repository root:

```bash
scripts/start-testsrv.sh
```

This one command does everything:

1. Creates a fresh database in a private temp dir
   (`/tmp/mudcroft-testsrv.XXXXXX`), loading the core library from `mudd/lib`.
2. Starts `mudd` on `127.0.0.1:8080`, serving the web client from
   `client/dist` on the same origin.
3. Logs in as the admin, creates the universe `test-world` ("Test Universe")
   and runs `scripts/cave_adventure.lua` in it.
4. Sets the spawn portal to `/rooms/cave-entrance`.

When it prints `READY FOR PLAY-TESTING`, open **http://127.0.0.1:8080**. Log in
as `admin` / `testpass123`, or register a new account (a `player`). Then pick
**Test Universe**.

The server runs in the foreground. Stop it with **Ctrl+C** in that terminal,
or with `scripts/stop-testsrv.sh` from another one. Either way the temp dir,
including the database and logs, is deleted.

| Environment variable | Default | Effect |
|----------------------|---------|--------|
| `PORT` | `8080` | Listen port. The Vite dev server proxies to 8080. |
| `ADMIN_USERNAME` | `admin` | Admin account name |
| `ADMIN_PASSWORD` | `testpass123` | Admin password |

The script also loads a `.env` file from the repository root, if one exists.
Put `VENICE_API_KEY=...` there to enable room images.

To work on the client with hot reload, keep the test server on port 8080, then
run `npm run dev` in `client/` and open http://localhost:3000.

## The Cave World

```
                  Treasure Chamber
                         | north
Cave Entrance --north--> Narrow Passage --east--> Underground Pool
```

| Room | Path | Contents |
|------|------|----------|
| Cave Entrance (portal) | `/rooms/cave-entrance` | Rusty Short Sword |
| Narrow Passage | `/rooms/narrow-passage` | Giant Bat (NPC, 15 HP) |
| Treasure Chamber | `/rooms/treasure-chamber` | Cave Troll (NPC, 50 HP), Ancient Gold Crown, Sapphire Necklace, Troll Slayer Sword |
| Underground Pool | `/rooms/underground-pool` | Glowing Mushroom |

Each exit has a matching way back: south from the Passage, south from the
Chamber, west from the Pool.

## Test Scenarios

### 1. Movement

- `look` (or `l`) shows the room. Any argument is ignored: `look sword` shows
  the room too.
- Move with `north`/`n`, `south`/`s`, `east`/`e`, `west`/`w`.
- Check that the room panel and exit list update, and that a missing exit
  replies `You can't go that way.`

### 2. Items

- At the entrance: `get sword` (or `take sword`). Names match partially and
  case-insensitively. Reply: `You take Rusty Short Sword.`
- `get bat` in the Narrow Passage replies `You can't take that`: NPCs and
  other players can't be picked up.
- `inventory` (or `i`, `inv`) lists what you carry.
- `drop sword` puts it back in the room.

### 3. Combat

- In the Narrow Passage: `attack bat` (or `kill bat`).
- Each command is a single swing: d20 against AC, then 1d6 damage on a hit,
  doubled on a natural 20 (`CRITICAL HIT!`). The reply shows a hit and the
  bat's remaining HP, a miss with the roll against AC, or a fumble. Damage
  varies from swing to swing.
- Repeat until `Giant Bat is slain!`. The bat then disappears from the room.
- There is no automatic combat round. NPCs do not strike back, and the client
  shows no player HP. The troll does not stop you from taking the treasure.

### 4. Talking and other players

Open a second browser (or a private window), register another account
(username 3-32 letters, digits, `_` or `-`; password at least 6 characters)
and enter the same universe.

- `say hello`: you see `You say: hello`, and the other player sees
  `<name> says: hello`.
- The other player also sees `<name> takes <item>.`, `<name> drops <item>.`
  and `<name> attacks <target>.`

### 5. Wizard commands (as `admin`)

- `help` shows the extra builder and wizard lines.
- `goto /rooms/treasure-chamber` teleports you. Others see you in the new
  room, and `get`/`drop` work there.
- `setportal` makes the current room the spawn point. Set it back with
  `setportal /rooms/cave-entrance`.
- `eval return game.get_object("/rooms/cave-entrance").name` replies
  `Cave Entrance`.
- `eval return game.create_object("/items/lamp", "item", "/rooms/cave-entrance", {name = "Brass Lamp"}).id`
  creates an item. The signature is `create_object(path, class, parent_id, props)`.
- `create item /items/rope "Coil of Rope" "Thirty feet of hemp." parent=/rooms/cave-entrance`
  is the builder command form.

### 6. Disconnect and reconnect

- Close the tab while carrying an item **outside** the entrance. After
  5 seconds your inventory is dropped where you stood.
- Log in again. You spawn at the portal, the entrance.
- Reconnecting within 5 seconds keeps everything.

### 7. Room images (needs `VENICE_API_KEY`)

- When a logged-in player first views a room that has no image, the server
  generates one in the background.
- The image is not pushed when it is ready: `look` again after a few seconds,
  and it appears in the room panel.

## Checklist

- [ ] Log in and register both work
- [ ] The universe list shows Test Universe
- [ ] The connection status shows connected
- [ ] You spawn at the Cave Entrance
- [ ] `look` and the direction commands update the room panel
- [ ] `get`, `drop` and `inventory` work
- [ ] `attack` damages and eventually kills the bat, which leaves the room
- [ ] `say` reaches a second player in the same room
- [ ] As admin: `goto`, `setportal` and `eval` work, and `help` lists them
- [ ] Room images appear (only if Venice is configured)

Server-wide known issues are listed in
[docs/roadmap.md](docs/roadmap.md#known-issues).

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `[ERROR] .../mudd not found` | `(cd mudd && cargo build --release)` |
| `[ERROR] client not built` | `(cd client && npm ci && npm run build)` |
| `[ERROR] Something is already listening on port 8080` | Stop the other server, or run `PORT=8081 scripts/start-testsrv.sh` |
| `[ERROR] A test server is already running` | `scripts/stop-testsrv.sh` |
| `setportal timed out` | Node is too old (no built-in `WebSocket`), or the server is unhealthy. Check `server.log`. |
| `Universe not initialized` or `You are nowhere.` | The portal step failed. As admin, run `setportal /rooms/cave-entrance`. |
| No room images | Put `VENICE_API_KEY` in `.env` at the repository root and restart |
| An `eval` fails | The Lua error comes back as an error message. Try smaller pieces, e.g. `eval return 1 + 1`. |

The server log is at `<temp dir>/server.log`; the start script prints the path.

## Automated Tests

```bash
scripts/e2e.sh                                   # whole suite
scripts/e2e.sh e2e/gameplay.spec.ts --headed     # one file, visible browser
```

`scripts/e2e.sh` builds `mudd` and the client, then runs the Playwright suite
in `client/e2e/` with headless Chromium. The suite starts its own `mudd` on a
random port in a temp dir. Set `MUDCROFT_E2E_KEEP=1` to keep the temp dir and
logs. The first time, install the browser with
`(cd client && npx playwright install chromium)`.
