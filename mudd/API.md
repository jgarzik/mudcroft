# HemiMUD Server API Reference

HTTP REST API and WebSocket protocol documentation.

---

## Base URL

```
http://localhost:8080
```

---

## Health & Info

### GET /

Server information.

**Response:**
```json
{
    "name": "mudd",
    "version": "0.1.0"
}
```

---

### GET /health

Health check endpoint.

**Response (200 OK):**
```json
{
    "status": "healthy",
    "database": "ok"
}
```

**Response (503 Service Unavailable):**
```json
{
    "status": "unhealthy",
    "database": "error"
}
```

---

## Authentication

### POST /auth/register

Create a new account.

**Request:**
```json
{
    "username": "player1",
    "password": "secretpassword123"
}
```

**Response (201 Created):**
```json
{
    "token": "eyJhbGciOiJIUzI1NiIs...",
    "account_id": "550e8400-e29b-41d4-a716-446655440000",
    "username": "player1",
    "access_level": "player"
}
```

**Response (409 Conflict):**
```json
{
    "error": "username already exists"
}
```

---

### POST /auth/login

Authenticate and get a JWT token.

**Request:**
```json
{
    "username": "player1",
    "password": "secretpassword123"
}
```

**Response (200 OK):**
```json
{
    "token": "eyJhbGciOiJIUzI1NiIs...",
    "account_id": "550e8400-e29b-41d4-a716-446655440000",
    "username": "player1",
    "access_level": "player"
}
```

**Response (401 Unauthorized):**
```json
{
    "error": "invalid credentials"
}
```

---

### POST /auth/logout

Invalidate a token.

**Request:**
```json
{
    "token": "eyJhbGciOiJIUzI1NiIs..."
}
```

**Response (200 OK):**
```json
{
    "success": true
}
```

---

### GET /auth/validate

Check if a token is valid.

**Query Parameters:**
- `token` (required): JWT token to validate

**Example:**
```
GET /auth/validate?token=eyJhbGciOiJIUzI1NiIs...
```

**Response (valid):**
```json
{
    "valid": true,
    "account_id": "550e8400-e29b-41d4-a716-446655440000",
    "username": "player1",
    "access_level": "player"
}
```

**Response (invalid):**
```json
{
    "valid": false
}
```

---

## WebSocket Protocol

### Connection

Connect to the WebSocket endpoint with optional authentication.

**Endpoint:**
```
ws://localhost:8080/ws?token=<jwt_token>
```

**Parameters:**
- `token` (optional): JWT token for authentication. If omitted, connects as guest.

**Example (wscat):**
```bash
wscat -c "ws://localhost:8080/ws?token=eyJhbGciOiJIUzI1NiIs..."
```

---

### Client → Server Messages

All messages are JSON with a `type` field.

#### Command

Send a player command.

```json
{
    "type": "command",
    "text": "look"
}
```

**Built-in Commands:**

| Command | Description |
|---------|-------------|
| `look` or `l` | View current room |
| `north` or `n` | Move north |
| `south` or `s` | Move south |
| `east` or `e` | Move east |
| `west` or `w` | Move west |
| `up` or `u` | Move up |
| `down` or `d` | Move down |
| `say <message>` | Speak in current room |
| `help` | Show available commands |
| `eval <lua>` | Execute Lua code (wizard+ only) |

#### Ping

Keep connection alive.

```json
{
    "type": "ping"
}
```

---

### Server → Client Messages

All messages are JSON with a `type` field.

#### Welcome

Sent immediately upon connection.

```json
{
    "type": "welcome",
    "player_id": "550e8400-e29b-41d4-a716-446655440000",
    "theme_id": "default"
}
```

#### Output

Text output to display.

```json
{
    "type": "output",
    "text": "You say: Hello everyone!"
}
```

#### Room

Room description with exits and contents.

```json
{
    "type": "room",
    "name": "Town Square",
    "description": "A bustling square at the heart of town.",
    "exits": ["north", "south", "east"],
    "contents": ["Iron Sword", "Wooden Shield"],
    "image_hash": "abc123..."
}
```

**Fields:**
- `name`: Room title
- `description`: Room description text
- `exits`: Array of available exit directions
- `contents`: Array of visible item/NPC names
- `image_hash`: (optional) Hash for `/images/{hash}` endpoint

#### Error

Error message.

```json
{
    "type": "error",
    "message": "Permission denied: wizard+ required for eval"
}
```

#### Echo

Command echo (confirmation of received command).

```json
{
    "type": "echo",
    "command": "look"
}
```

---

### WebSocket Flow Example

```
Client                              Server
  |                                   |
  |------ WS Connect w/ token ------->|
  |                                   |
  |<-------- Welcome message ---------|
  |<-------- Room description --------|
  |                                   |
  |-------- {"type":"command",------->|
  |          "text":"look"}           |
  |                                   |
  |<-------- Echo: "look" ------------|
  |<-------- Room description --------|
  |                                   |
  |-------- {"type":"command",------->|
  |          "text":"north"}          |
  |                                   |
  |<-------- Echo: "north" -----------|
  |<-------- New room description ----|
  |                                   |
  |-------- {"type":"ping"} --------->|
  |                                   |
  |-------- Connection closed ------->|
```

---

## Universe Management

### POST /universe/create

Create a new universe from JSON.

**Request:**
```json
{
    "id": "my-universe",
    "name": "My Adventure World",
    "owner_id": "account-uuid",
    "config": {
        "pvp_enabled": false,
        "spawn_room": "starting-room-id"
    },
    "libs": {
        "combat": "Combat = { ... lua code ... }",
        "commands": "Commands = { ... lua code ... }"
    }
}
```

**Fields:**
- `id` (optional): Universe ID (auto-generated UUID if omitted)
- `name` (required): Display name
- `owner_id` (required): Account ID of owner
- `config` (optional): Configuration object
- `libs` (optional): Map of library name to Lua source code

**Response (201 Created):**
```json
{
    "id": "my-universe",
    "name": "My Adventure World",
    "libs_loaded": ["combat", "commands"]
}
```

**Response (400 Bad Request):**
```json
{
    "error": "Failed to create universe: ..."
}
```

---

### POST /universe/upload

Create a universe from a ZIP file.

**Request:**
- Content-Type: `application/octet-stream`
- Body: ZIP file bytes

**ZIP Structure:**
```
universe.zip
├── universe.json       # Required: universe config
└── lib/
    ├── combat.lua      # Optional: Lua libraries
    ├── commands.lua
    └── items.lua
```

**universe.json format:**
```json
{
    "id": "optional-id",
    "name": "My Universe",
    "owner_id": "account-uuid",
    "config": { ... }
}
```

**Response (201 Created):**
```json
{
    "id": "generated-uuid",
    "name": "My Universe",
    "libs_loaded": ["combat", "commands", "items"]
}
```

**Response (400 Bad Request):**
```json
{
    "error": "Invalid ZIP file: ..."
}
```

```json
{
    "error": "Missing universe.json in ZIP file"
}
```

---

## Images

### GET /images/{hash}

Retrieve a stored image by content hash.

**Parameters:**
- `hash`: SHA256 hash of the image content

**Response (200 OK):**
- Content-Type: `image/png` (or appropriate MIME type)
- Cache-Control: `public, max-age=31536000, immutable`
- Body: Image bytes

**Response (404 Not Found):**
```
Image not found
```

**Usage:**
Images are referenced by hash in room descriptions (via `image_hash` field) or generated via Venice AI (`game.llm_image()`).

---

## Error Responses

All error responses follow this format:

```json
{
    "error": "Description of what went wrong"
}
```

### HTTP Status Codes

| Code | Meaning |
|------|---------|
| 200 | Success |
| 201 | Created (successful creation) |
| 400 | Bad Request (invalid input) |
| 401 | Unauthorized (invalid credentials) |
| 404 | Not Found |
| 409 | Conflict (e.g., username exists) |
| 500 | Internal Server Error |
| 503 | Service Unavailable |

---

## Access Levels

Account access levels control permissions:

| Level | Description |
|-------|-------------|
| `player` | Default level, can play and interact |
| `builder` | Can create/modify in assigned regions |
| `wizard` | Full object control, can use `eval` |
| `admin` | Universe config, grant credits |
| `owner` | Can grant admin access |

Access level is returned in auth responses and stored in the account.

---

## Rate Limits

The server does not currently implement rate limiting. For production deployments, consider placing a reverse proxy (nginx, Cloudflare) in front of the server.

---

## CORS

CORS is not enabled by default. For web client access, configure a reverse proxy or modify the server.

---

## Example: Full Session

```bash
# 1. Register a new account
curl -X POST http://localhost:8080/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"hero","password":"password123"}'

# Response:
# {"token":"eyJ...","account_id":"abc123","username":"hero","access_level":"player"}

# 2. Connect via WebSocket
wscat -c "ws://localhost:8080/ws?token=eyJ..."

# Server sends:
# {"type":"welcome","player_id":"def456","theme_id":"default"}
# {"type":"room","name":"Starting Room","description":"...","exits":["north"],"contents":[]}

# 3. Send commands
> {"type":"command","text":"look"}
< {"type":"echo","command":"look"}
< {"type":"room","name":"Starting Room",...}

> {"type":"command","text":"north"}
< {"type":"echo","command":"north"}
< {"type":"room","name":"Forest Path",...}

# 4. Logout
curl -X POST http://localhost:8080/auth/logout \
  -H "Content-Type: application/json" \
  -d '{"token":"eyJ..."}'
```

---

## CLI Commands

### Initialize Database

```bash
export MUDD_ADMIN_USERNAME=admin
export MUDD_ADMIN_PASSWORD=secretpassword123
mudd_init --database /path/to/game.db
```

### Start Server

```bash
mudd --database /path/to/game.db --bind 127.0.0.1:8080
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `RUST_LOG` | Log level | `mudd=info,tower_http=debug` |
| `MUDD_ADMIN_USERNAME` | Admin username (init only) | required |
| `MUDD_ADMIN_PASSWORD` | Admin password (init only) | required |
