# Administrator Guide

## Installation

### From Source

```bash
git clone <repo>
cd mudcroft/mudd
cargo build --release

# Binaries
cp target/release/mudd /usr/local/bin/
cp target/release/mudd_init /usr/local/bin/
```

### Verify Installation

```bash
mudd --version
mudd_init --version
```

## Database Initialization

Initialize once before first server start.

```bash
export MUDD_ADMIN_USERNAME=admin
export MUDD_ADMIN_PASSWORD=YourSecurePassword123

mudd_init --database /var/lib/mudd/game.db
```

### With Lua Libraries

```bash
mudd_init --database /var/lib/mudd/game.db \
  --lib /path/to/combat.lua \
  --lib /path/to/commands.lua
```

Libraries stored in `code_store` table by SHA256 hash.

### Requirements

- Database path must not exist
- Password minimum 8 characters
- Parent directory must exist and be writable

## Server Configuration

### Basic Startup

```bash
mudd --database /var/lib/mudd/game.db --bind 0.0.0.0:8080
```

### Logging

Control via `RUST_LOG` environment variable:

```bash
# Debug mode
RUST_LOG=mudd=debug mudd --database /var/lib/mudd/game.db

# Quiet mode
RUST_LOG=mudd=warn mudd --database /var/lib/mudd/game.db

# Component-specific
RUST_LOG=mudd::api=debug,mudd::lua=trace mudd --database /var/lib/mudd/game.db
```

### Systemd Service

```ini
# /etc/systemd/system/mudd.service
[Unit]
Description=HemiMUD Server
After=network.target

[Service]
Type=simple
User=mudd
ExecStart=/usr/local/bin/mudd --database /var/lib/mudd/game.db --bind 0.0.0.0:8080
Restart=on-failure
Environment=RUST_LOG=mudd=info

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable mudd
systemctl start mudd
```

## Creating Universes

Universe IDs must be DNS-style identifiers:
- 3-64 characters
- Lowercase alphanumeric and hyphens only
- Must start and end with alphanumeric
- No consecutive hyphens (`--`)

Examples: `my-game`, `test123`, `rpg-world-2`

### Via JSON API

```bash
curl -X POST http://localhost:8080/universe/create \
  -H "Content-Type: application/json" \
  -d '{
    "id": "my-game",
    "name": "My World",
    "owner_id": "<admin_account_id>",
    "config": {
      "pvp_policy": "disabled"
    },
    "libs": {
      "combat": "-- combat.lua content",
      "commands": "-- commands.lua content"
    }
  }'
```

Response:
```json
{
  "id": "my-game",
  "name": "My World",
  "libs_loaded": ["combat", "commands"]
}
```

### Via ZIP Upload

Create ZIP with structure:
```
universe.zip
├── universe.json
└── lib/
    ├── combat.lua
    └── commands.lua
```

`universe.json`:
```json
{
  "id": "my-game",
  "name": "My World",
  "owner_id": "<admin_account_id>",
  "config": {}
}
```

Upload:
```bash
curl -X POST http://localhost:8080/universe/upload \
  -H "Content-Type: application/octet-stream" \
  --data-binary @universe.zip
```

## Universe Initialization

After creating a universe and uploading Lua libraries, wizards must initialize it by setting a spawn portal. Until a portal is set, players see "Universe not initialized" when connecting.

### Workflow

1. **Upload universe** - Create universe via API or ZIP upload
2. **Connect as wizard** - Log in with wizard+ access level
3. **Run init script** - Execute Lua to create rooms:
   ```
   eval return init()
   ```
4. **Teleport to entrance** - Use `goto` to find your spawn room:
   ```
   goto <room_id>
   ```
5. **Set portal** - Designate current room as spawn point:
   ```
   setportal
   ```

### Wizard Commands

| Command | Description |
|---------|-------------|
| `goto <room_id>` | Teleport to any room by ID |
| `setportal` | Set portal to current room |
| `setportal <room_id>` | Set portal to specified room |

### Portal Storage

Portal is stored in `universe_settings` table:

```sql
-- Check current portal
SELECT * FROM universe_settings WHERE universe_id = 'my-game' AND key = 'portal_room_id';

-- Manually set portal (emergency use)
INSERT OR REPLACE INTO universe_settings (universe_id, key, value)
VALUES ('my-game', 'portal_room_id', '<room_uuid>');
```

### Universe Settings API

The `universe_settings` table provides key-value storage per universe:

| Column | Type | Description |
|--------|------|-------------|
| universe_id | TEXT | Foreign key to universes |
| key | TEXT | Setting name |
| value | TEXT | Setting value |

Currently used keys:
- `portal_room_id` - UUID of spawn room

## Account Management

### Create Account

```bash
curl -X POST http://localhost:8080/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username": "player1", "password": "password123"}'
```

### Login

```bash
curl -X POST http://localhost:8080/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "player1", "password": "password123"}'
```

Response includes `token` for WebSocket auth.

### Validate Token

```bash
curl "http://localhost:8080/auth/validate?token=<token>"
```

### Logout

```bash
curl -X POST http://localhost:8080/auth/logout \
  -H "Content-Type: application/json" \
  -d '{"token": "<token>"}'
```

## Access Levels

| Level | Value | Capabilities |
|-------|-------|--------------|
| Player | 0 | Normal gameplay, interact with non-fixed objects |
| Builder | 1 | Create/modify objects in assigned regions |
| Wizard | 2 | Full object control, `eval` command, bypass fixed |
| Admin | 3 | Universe config, grant credits |
| Owner | 4 | Grant admin access, full control |

### Set Access Level

Via Lua `eval` as wizard+:
```
eval game.set_access_level("account_id", "wizard")
```

Or direct SQL:
```sql
UPDATE accounts SET access_level = 'wizard' WHERE username = 'player1';
```

### Assign Builder Regions

Via Lua:
```
eval game.assign_region("account_id", "region_id")
```

## Monitoring

### Health Endpoint

```bash
curl http://localhost:8080/health
```

Response:
```json
{
  "status": "healthy",
  "database": "ok"
}
```

Status codes:
- 200: Healthy
- 503: Unhealthy (database error)

### Server Info

```bash
curl http://localhost:8080/
```

Response:
```json
{
  "name": "mudd",
  "version": "0.1.0"
}
```

## Backup and Restore

### SQLite Backup

Database uses WAL mode. Safe backup methods:

```bash
# Method 1: sqlite3 backup command
sqlite3 /var/lib/mudd/game.db ".backup /backup/game.db.bak"

# Method 2: Copy during quiescence
systemctl stop mudd
cp /var/lib/mudd/game.db /backup/
cp /var/lib/mudd/game.db-wal /backup/
cp /var/lib/mudd/game.db-shm /backup/
systemctl start mudd
```

### Restore

```bash
systemctl stop mudd
cp /backup/game.db /var/lib/mudd/game.db
# Remove WAL files to force recovery
rm -f /var/lib/mudd/game.db-wal /var/lib/mudd/game.db-shm
systemctl start mudd
```

## Troubleshooting

### Database Not Found

```
Error: Database file not found: /path/to/game.db. Run mudd_init to create it.
```

Solution: Run `mudd_init` first or check path.

### Database Already Exists

```
Error: Database file already exists: /path/to/game.db
```

Solution: Remove existing file or use different path.

### Missing Admin Account

```
Error: Database has no admin account. Run mudd_init to create a valid database.
```

Solution: Re-initialize database.

### Password Too Short

```
Error: Admin password must be at least 8 characters
```

Solution: Use longer `MUDD_ADMIN_PASSWORD`.

### Port Already in Use

```
Error: error binding to 127.0.0.1:8080: Address already in use
```

Solution: Use different `--bind` address or stop conflicting process.

### Lua Instruction Limit

```
Lua error: instruction limit exceeded (1001000 > 1000000)
```

Solution: Optimize Lua code or use wizard eval with higher limits (10M instructions).

### Permission Denied

```
Permission denied: wizard+ required for eval
```

Solution: Promote account to wizard+ access level.

## Database Schema Reference

Key tables for administrative queries:

```sql
-- List accounts
SELECT id, username, access_level, created_at FROM accounts;

-- List universes
SELECT id, name, owner_id, created_at FROM universes;

-- Count objects per universe
SELECT universe_id, COUNT(*) FROM objects GROUP BY universe_id;

-- List builder assignments
SELECT account_id, region_id FROM builder_regions;

-- Check timers
SELECT id, object_id, method, fire_at FROM timers ORDER BY fire_at;

-- Credit balances
SELECT player_id, balance FROM credits WHERE universe_id = '<id>';

-- Universe settings (including portal)
SELECT * FROM universe_settings WHERE universe_id = '<id>';
```
