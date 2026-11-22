# Mudcroft Server API Reference

The HTTP REST API and WebSocket protocol served by `mudd`. For command-line
flags and environment variables, see [admin.md](admin.md#command-line-reference).
For access levels, see [permissions.md](permissions.md#access-levels).

## Conventions

- **Base URL:** `http://127.0.0.1:8080`, the default `--bind`.
- **Request bodies** are JSON (`Content-Type: application/json`), except
  `/universe/upload`, which takes raw ZIP bytes.
- **Authentication:** `POST /auth/login` or `/auth/register` returns a token.
  Authenticated HTTP endpoints take it as `Authorization: Bearer <token>`. The
  WebSocket takes it as the `token` query parameter.
- **Tokens** are opaque: 64 random hex characters stored on the account row.
  They are not JWTs and do not expire. Each account has one token at a time:
  a new login replaces it, and logout clears it.
- **Passwords** are stored as argon2id hashes (PHC strings with a random
  salt) and verified in constant time; a login for an unknown username takes
  as long as one with a wrong password. At most one argon2 operation per CPU
  core runs at a time; further logins wait.
- **Errors** are JSON `{"error": "<message>"}`, with these exceptions, which
  return plain text:
  - `GET /images/{hash}` failures
  - WebSocket upgrade failures (`/ws`)
  - Requests the framework rejects before a handler runs: a missing or wrong
    `Content-Type` (415), malformed or incomplete JSON (400 or 422), or a
    missing required query parameter (400)
- **Clusters:** any node answers. A node that is not the primary forwards
  `/auth/*` and `/universe/*` to it and relays game connections; it answers
  `503` (plain text) when there is no primary at the moment, and `502` when
  the primary did not answer. Retry either.
- **Every response** carries `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: SAMEORIGIN` and
  `Referrer-Policy: strict-origin-when-cross-origin`, and text bodies are
  gzip-compressed when the request sends `Accept-Encoding: gzip`, so `mudd`
  needs no proxy in front for these.

## Server

### GET /

Server name and version:

```json
{"name": "mudd", "version": "0.1.0"}
```

This route exists only when `mudd` runs **without** `--static-dir`. With
`--static-dir`, `/` and every path no API route claims serve the web client
(files from the directory, otherwise `index.html`).

### GET /health

| Status | Body |
|--------|------|
| 200 | `{"status": "healthy", "database": "ok"}` |
| 503 | `{"status": "unhealthy", "database": "error"}` |

## Authentication

### POST /auth/register

Creates a `player` account and logs it in. Usernames must be 3-32 ASCII
letters, digits, `_` or `-`; passwords must be 6 characters to 1 KiB.

```json
{"username": "player1", "password": "secretpassword123"}
```

| Status | Body |
|--------|------|
| 201 | `{"token": "<64 hex>", "account_id": "<uuid>", "username": "player1", "access_level": "player"}` |
| 400 | `{"error": "username must be 3-32 characters"}`, `{"error": "username may contain only letters, digits, _ and -"}` `{"error": "password must be at least 6 characters"}` or `{"error": "password must be at most 1024 bytes"}` |
| 409 | `{"error": "username already exists"}` |
| 500 | `{"error": "database error: ..."}` |

### POST /auth/login

```json
{"username": "player1", "password": "secretpassword123", "device_token": "<optional>"}
```

| Status | Body |
|--------|------|
| 200 | `{"token": "<64 hex>", "account_id": "<uuid>", "username": "player1", "access_level": "player", "device_token": "<64 hex>"}` |
| 401 | `{"error": "invalid credentials"}` (unknown user or wrong password) |

Logging in issues a new token and invalidates the account's previous one.

`device_token` (also returned by registration) proves this device has
logged in to the account before: send it with the next login, which returns
a fresh one in its place. Tokens last 30 days and are kept in the server's
memory only (a restart or failover forgets them; the device then gets a new
one at its next login). A token presented with a wrong password is revoked.
An account keeps at most 10 (its oldest goes); when the server holds as many
as it allows, new devices are simply not remembered.

Limited to `--auth-limit` failed logins a minute per username and client
(default 30), and twice that many login and registration requests per
client (an IPv4 address or an IPv6 /64): beyond that,
`429 {"error": "too many attempts; try again later"}` with a `Retry-After`
header (seconds), before the password is checked. A successful login does
not count, and one client never blocks another. Beyond ten times the rate of
failures for one username across all clients, attempts for it go one at a
time, a second apart, except logins presenting a valid `device_token` for
the account, which go straight through (the owner's devices are never held
up by an attack); beyond a hundred times the rate they are refused.

### POST /auth/logout

```json
{"token": "<token>", "device_token": "<optional>"}
```

Response 200: `{"success": true}`, or `{"success": false}` if no account had
that token. A `device_token` given is revoked.

### GET /auth/validate?token=\<token\>

Always returns 200:

```json
{"valid": true, "account_id": "<uuid>", "username": "player1", "access_level": "player"}
```

```json
{"valid": false}
```

## Universes

### GET /universe/list

Lists every universe. Any valid token works.

```bash
curl -s http://127.0.0.1:8080/universe/list -H "Authorization: Bearer $TOKEN"
```

```json
[{"id": "test-world", "name": "Test Universe"}]
```

### POST /universe/create

Creates a universe. Requires an **admin+** token.

```json
{
  "id": "my-game",
  "name": "My Adventure World",
  "owner_id": "<account_id>",
  "config": {"motd": "Welcome!"},
  "libs": {"quests": "Quests = {}"}
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `id` | yes | Universe id. DNS-style: 3-64 characters, `[a-z0-9-]`, starts and ends alphanumeric, no `--`. Uppercase is lowercased. |
| `name` | yes | Display name |
| `owner_id` | yes | An existing account id. That account is `owner` inside this universe (see [permissions.md](permissions.md#access-levels)). |
| `config` | no | JSON object, stored as-is plus the server-set `lib_hashes` key. Lua reads it with `game.get_universe().config`. |
| `libs` | no | Map of library name to Lua source. These are added to the core libraries from `mudd_init`, and override any with the same name. |

| Status | Body |
|--------|------|
| 201 | `{"id": "my-game", "name": "My Adventure World", "libs_loaded": ["combat", "commands", "quests", "utils"]}` |
| 400 | `{"error": "Invalid universe ID: ..."}` or `{"error": "Failed to create universe: ..."}` (duplicate id, unknown `owner_id`) |
| 401 | Missing, malformed or invalid token |
| 403 | `{"error": "Admin access required"}` |

`libs_loaded` lists the core libraries in sorted order, followed by any new
names from `libs`.

### POST /universe/upload

Creates a universe from a ZIP file. Requires an **admin+** token.

```
universe.zip
├── universe.json   # required: {"id", "name", "owner_id", "config"?}, same rules as /universe/create
└── lib/
    └── quests.lua  # every *.lua file in the ZIP becomes a library named after its file stem
```

```bash
curl -s -X POST http://127.0.0.1:8080/universe/upload \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/octet-stream' \
    --data-binary @universe.zip
```

Responses are the same as for `/universe/create`, plus these 400 errors:
`Invalid ZIP file: ...`, `Missing universe.json in ZIP file` and
`Invalid universe.json: ...`.

### POST /universe/{id}/run_script

Runs a Lua file from the server's `scripts/` directory inside a universe.

```json
{"script": "cave_adventure.lua", "account_id": "<optional account id>"}
```

- **Who may call:** an account that is admin+ in universe `{id}`: a global
  admin or owner, an admin member, or the universe's owner.
- **Runs as:** the authenticated caller. If `account_id` names another account,
  the caller must be admin+ **and** outrank that account in universe `{id}`
  (a global admin or owner only by a higher global level; see
  [permissions.md](permissions.md#access-levels)). Otherwise
  the response is 403. Objects the script creates are owned by the account it
  runs as.
- **Script path:** read from `scripts/<script>`, relative to `mudd`'s working
  directory. Names containing `..` or starting with `/` are rejected.
- **Environment:** the universe's libraries are loaded first. The script gets
  the larger eval budget (10M instructions, 5 s, 10,000 DB calls), and any
  `game.send` / `game.broadcast` messages it queues are delivered.
- `{id}` is lowercased before lookup, as for `/universe/create` and `/ws`.

| Status | Body |
|--------|------|
| 200 | `{"result": "<the script's return value as text>"}`. A table renders as `[table]`, nothing as `nil`. |
| 400 | `{"error": "Invalid universe ID: ..."}`, `{"error": "Invalid script path"}` or `{"error": "Script error: ..."}` |
| 401 | Missing, malformed or invalid token |
| 403 | `{"error": "Admin access or universe ownership required"}` or `{"error": "Cannot run a script as that account"}` |
| 404 | `{"error": "Universe not found: <id>"}` or `{"error": "Script not found: <name> (...)"}` |

### POST /universe/{id}/portal

Sets the universe's portal: the room players spawn in.

```json
{"room_id": "/rooms/cave-entrance"}
```

- **Who may call:** admin+ in universe `{id}`, as for `run_script`.
- The room must be an object of class `room` in universe `{id}`.
- `{id}` is lowercased before lookup.

| Status | Body |
|--------|------|
| 200 | `{"universe_id": "<id>", "room_id": "<room>"}` |
| 400 | `{"error": "Invalid universe ID: ..."}` or `{"error": "<room> is not a room"}` |
| 401 | Missing, malformed or invalid token |
| 403 | `{"error": "Admin access or universe ownership required"}` |
| 404 | `{"error": "Universe not found: <id>"}` or `{"error": "Room not found: <room>"}` |

## Images

### GET /images/{hash}

Returns a stored image by the hex SHA-256 of its bytes.

- **200:** the image bytes, with `Content-Type` set to the stored MIME type
  and `Cache-Control: public, max-age=31536000, immutable`
- **404:** plain text `Image not found`
- **500:** plain text `Error: ...`

Room messages carry `image_hash` when a room has an image. If `VENICE_API_KEY`
is set, the server generates a missing room image in the background when a
logged-in player views the room.

## WebSocket Protocol

### Connecting

```
ws://127.0.0.1:8080/ws?universe=<universe_id>&token=<token>
```

| Parameter | Required | Description |
|-----------|----------|-------------|
| `universe` | yes | Universe id (lowercased before lookup) |
| `token` | yes | Session token from `/auth/login` or `/auth/register`. There are no guests. |

Upgrade failures are plain-text HTTP responses:

| Status | Body |
|--------|------|
| 400 | `Universe ID required in query param: ?universe=<id>` or `Invalid universe ID: ...` |
| 401 | `A valid token is required: ?token=<token>` (missing, invalid or logged-out token) |
| 404 | `Universe not found: <id>` |
| 500 | `Failed to verify universe` |

Every player has a persistent player object in the universe,
`/players/p-<account_id>`, which holds their inventory. A player spawns at
their `last_safe_location`. If that is not set, they spawn in the room they
were last in, if it is a safe zone (the portal, a room with `is_portal = true`,
or their workroom). Otherwise they spawn at the universe portal.

One player has one connection per universe: a new connection replaces the
old one, which receives `replaced`. When a player disconnects, the server
waits 5 seconds for a reconnect. If the player does not return and is outside a safe zone, their
inventory is dropped in the room.

### Client to server

Every message is JSON with a `type` field.

```json
{"type": "command", "text": "look"}
```

A command may carry an `id` (up to 64 characters): the server answers
`{"type": "done", "id": ...}` once the command is saved, and a command
whose id was already saved (in the last ten minutes) is not run again, only
answered `done`. Gateways use this to retry a command interrupted by a
failover; a client may use it the same way.

```json
{"type": "ping"}
```

```json
{"type": "confirm", "id": "<payment id>", "accept": true}
```

`confirm` answers a payment prompt (see `confirm` below); typing the command
`yes` or `no` answers the latest one.

`ping` gets no reply. Messages that are not valid JSON, or have an unknown
`type`, are ignored.

### Server to client

**welcome**: sent first, on connect.

```json
{"type": "welcome", "player_id": "/players/p-<account_id>", "theme_id": "sierra-retro"}
```

`theme_id` is always `sierra-retro` for now.

**room**: the room description. Sent after `welcome` if the player has a
spawn room, and in reply to `look` and to movement.

```json
{
  "type": "room",
  "name": "Cave Entrance",
  "description": "A dark opening in the mountainside...",
  "exits": ["north"],
  "contents": ["Rusty Short Sword"],
  "image_hash": "9f86d08..."
}
```

`exits` holds the keys of the room's `exits` property. `contents` holds the
`name` of every named object in the room, including NPCs and players. Your own
name is included when you are logged in. `image_hash` is omitted when the room
has no image.

**output**: text to display. This covers command replies, `say` from others,
and messages sent by Lua (`game.send`, `game.broadcast`).

```json
{"type": "output", "text": "You say: hello"}
```

**error**: a command failed.

```json
{"type": "error", "message": "Permission denied: wizard+ required for eval"}
```

**echo**: sent for every command, before its reply.

```json
{"type": "echo", "command": "look"}
```

**replaced**: a newer connection for the same player and universe (another
tab or device) has taken over, and the server closes this one right after.
Clients must not reconnect automatically on it, or the two connections would
keep evicting each other.

```json
{"type": "replaced", "message": "You connected from another window; this one is now disconnected."}
```

**done**: the command sent with this `id` is saved (see Client to server).

```json
{"type": "done", "id": "cmd-1"}
```

**confirm**: object code asks the player to pay (`game.request_payment`).
Nothing is charged unless the client answers with `{"type": "confirm", "id",
"accept": true}` (or the player types `yes`) within a minute; the reply is an
`output` (the object's answer, or a decline/expiry message) or an `error`
(no such request).

```json
{"type": "confirm", "id": "<payment id>", "text": "Pay 3 credits for an oil lamp?", "amount": 3}
```

If no spawn room exists, the server sends this right after `welcome`:
`{"type":"output","text":"Universe not initialized. Wizards: use 'setportal' command."}`

### Commands

Verbs are case-insensitive. `help` lists only the commands available at your
access level.

| Command | Level | Description |
|---------|-------|-------------|
| `look`, `l` | any | Show the current room. Any argument is ignored. |
| `north`/`n`, `south`/`s`, `east`/`e`, `west`/`w`, `up`/`u`, `down`/`d` | any | Move through an exit. Runs `on_leave` on the old room and `on_enter` on the new one. |
| `say <message>` | any | You see `You say: <message>`. Others in the room see `<name> says: <message>`. |
| `help` | any | List commands |
| `get <item>`, `take <item>` | any | Pick up an object in the room. Name matching is partial and case-insensitive; `%` and `_` match literally. Objects with `fixed = true` and living things (NPCs, players) cannot be taken. |
| `drop <item>` | any | Drop an object you carry |
| `inventory`, `inv`, `i` | any | List what you carry |
| `attack <target>`, `kill <target>` | any | One attack on an object in the room of class `npc` (or a class derived from it) or `monster`: d20 to hit, then 1d6 physical damage, doubled on a natural 20. Shows the damage and the target's remaining HP. At 0 HP the target is removed from the room. Targets do not fight back. |
| `create <class> <path> "<name>" ["<description>"] [key=value ...]` | builder+ | Create an object you own. `parent=<id>` sets its container. Other `key=value` pairs become properties: numbers and `true`/`false` are typed, everything else is a string. Needs a path grant covering `<path>`, or wizard+. |
| `eval <lua>` | wizard+ | Run Lua as yourself with the eval budget. The code is everything after the verb (leading whitespace is fine). The reply is the return value as text (`[table]` for tables), even when it is empty. |
| `goto <room_id>` | wizard+ | Teleport to a room in this universe. Runs `on_leave`/`on_enter` and moves your player object too, so `get`, `drop` and reconnects use the new room. |
| `setportal [room_id]` | wizard+ | Set this universe's spawn room: the given room, or your current one |

Any other verb is offered to custom verbs registered with `game.add_action`.
The candidates, in order, are: room actions, then actions on objects you carry,
on objects in the room, and on the room itself. If you name a target
(`pull lever`), an object whose name matches wins. The handler runs as the
object's owner. A non-empty string result becomes the reply. A `nil` or `""` result sends
no reply, which is useful when the handler already used `game.send`. If no verb
matches: `{"type":"output","text":"Unknown command: <verb>"}`.

Players below wizard see `Something went wrong.` when a custom verb's handler
fails. Wizards see the Lua error.

### Delivery

Each connection has an outgoing queue of 256 messages. If a client stops
reading and its queue fills, further messages **to that client** are dropped
(and logged). Other players are not affected.

### Example session

```
→ (connect) ws://127.0.0.1:8080/ws?universe=test-world&token=<token>
← {"type":"welcome","player_id":"/players/p-…","theme_id":"sierra-retro"}
← {"type":"room","name":"Cave Entrance","description":"…","exits":["north"],"contents":["Rusty Short Sword","hero"]}
→ {"type":"command","text":"take sword"}
← {"type":"echo","command":"take sword"}
← {"type":"output","text":"You take Rusty Short Sword."}
→ {"type":"command","text":"n"}
← {"type":"echo","command":"n"}
← {"type":"room","name":"Narrow Passage","description":"…","exits":["south","north","east"],"contents":["Giant Bat","hero"]}
```

## HTTP Status Codes

| Code | Meaning |
|------|---------|
| 200 | Success |
| 201 | Created (`/auth/register`, `/universe/create`, `/universe/upload`) |
| 400 | Invalid input: bad username or password on register, bad universe id, bad ZIP, script error, malformed JSON |
| 401 | Missing, malformed or invalid token, or wrong login credentials |
| 403 | Valid token, insufficient access (not admin, not universe owner, cannot run as that account) |
| 404 | Unknown universe, script or image |
| 409 | Username already exists |
| 415 | Missing or wrong `Content-Type` on a JSON endpoint (plain text) |
| 422 | JSON that does not match the expected fields (plain text) |
| 500 | Internal error |
| 503 | `/health`: the database is unavailable |

## Example: Full Session

```bash
SERVER=http://127.0.0.1:8080

# 1. Register (or log in) and keep the token
TOKEN=$(curl -s -X POST $SERVER/auth/register \
    -H 'Content-Type: application/json' \
    -d '{"username":"hero","password":"password123"}' | jq -r .token)

# 2. Find a universe
curl -s $SERVER/universe/list -H "Authorization: Bearer $TOKEN"

# 3. Play over WebSocket (wscat: npm install -g wscat)
wscat -c "ws://127.0.0.1:8080/ws?universe=test-world&token=$TOKEN"
> {"type":"command","text":"look"}

# 4. Log out
curl -s -X POST $SERVER/auth/logout \
    -H 'Content-Type: application/json' \
    -d "{\"token\":\"$TOKEN\"}"
```

## Not Provided

- **CORS:** no CORS headers are sent. Serve the web client from the same origin
  with `mudd --static-dir`, use the Vite dev server's proxy
  (`client/vite.config.ts`), or put a reverse proxy in front.
- **Rate limiting:** none. Use a reverse proxy if you need it.
- **TLS:** none. Terminate TLS in a reverse proxy.
