# Administrator Guide

How to install, configure, run and maintain a Mudcroft server. This page is
the canonical reference for the `mudd` and `mudd_init` command-line flags and
environment variables.

Related references:

- [API.md](API.md): HTTP and WebSocket API
- [permissions.md](permissions.md): access levels and the permission model
- [SCHEMA.md](../../SCHEMA.md): database tables

## Installation

### From source

```bash
git clone <repo-url> mudcroft
cd mudcroft/mudd
cargo build --release
```

This builds three binaries:

- `target/release/mudd`: the server
- `target/release/mudd_init`: creates and upgrades the database
- `target/release/mudd_admin`: offline administration (global access levels)

To serve the web client from `mudd`, also build it:

```bash
cd ../client
npm ci
npm run build        # produces client/dist
```

To install system-wide, copy the binaries and also the core Lua library
(`mudd/lib/*.lua`), because `mudd_init` must be able to read it:

```bash
sudo install -m 755 target/release/mudd target/release/mudd_init target/release/mudd_admin /usr/local/bin/
sudo mkdir -p /usr/local/share/mudcroft
sudo cp -r lib /usr/local/share/mudcroft/lib
sudo cp -r ../client/dist /usr/local/share/mudcroft/www    # optional
```

Check the install:

```bash
mudd --version
mudd_init --version
```

## Command-Line Reference

### mudd_init

Creates a new database, or upgrades an existing one in place. Running it again
on the same database is safe. Schema upgrades live here, never in `mudd`:
the database records its schema version, `mudd` refuses a mismatch, and
`mudd_init` upgrades older versions. There are no upgrade steps yet, so a
database from before versioning is refused (create a new one); otherwise a
re-run creates any missing tables and refreshes the core mudlib.

```
mudd_init --database <PATH> [--lib-dir <DIR>] [--lib <FILE.lua>]... [--force]
```

| Flag | Description |
|------|-------------|
| `-d`, `--database <PATH>` | SQLite database file. Created if missing, upgraded if present. Required. |
| `--lib-dir <DIR>` | Core mudlib directory. Every `*.lua` file in it is stored and becomes a default library for new universes. Default: `lib` **relative to the current directory**. |
| `--lib <FILE>` | Extra Lua file to store in `code_store` (repeatable). It is stored by hash only. It is not attached to any universe. |
| `-f`, `--force` | Delete the existing database file first, then create a fresh one |
| `-V`, `--version` | Print the version |

| Environment variable | Description |
|----------------------|-------------|
| `MUDD_ADMIN_USERNAME` | Username of the first account, created at the `admin` level. Required for a new database, ignored on upgrade. |
| `MUDD_ADMIN_PASSWORD` | Its password, at least 8 characters. Required for a new database, ignored on upgrade. |
| `RUST_LOG` | Log filter. Default `mudd=info`. |

> **Important: core library location.** The core commands (`get`, `drop`,
> `inventory`) and the combat helpers live in `mudd/lib/*.lua`. Without
> `--lib-dir`, `mudd_init` looks for `./lib`. If that directory does not exist,
> it logs `No core mudlib directory found at lib (skipping)` and continues.
> Universes created afterwards then have no core library, and those commands
> fail with Lua errors. Either run `mudd_init` from the `mudd/` directory or
> pass `--lib-dir` explicitly:
>
> ```bash
> mudd_init --database /var/lib/mudd/game.db --lib-dir /path/to/mudcroft/mudd/lib
> ```

Each universe records the library hashes current **when it is created**, in
`universes.config.lib_hashes`. Re-running `mudd_init` with newer library files
affects only universes created after that.

### mudd_admin

Offline administration. Like `mudd_init` it writes the database directly
(outside Raft and the server's caches), so **stop `mudd` first**.

```
mudd_admin --database <PATH> set-level <USERNAME> <admin|owner|none>
```

`set-level` sets an account's global level: `admin` or `owner` apply in
every universe; `none` removes the global level (per-universe levels, set
with `game.set_access_level`, are unaffected). It refuses to remove the last
global admin or owner, and exits non-zero on an unknown account.

### mudd

Runs the server. The database must already exist (see `mudd_init`).

```
mudd --database <PATH> [--bind <ADDR>] [--static-dir <DIR>]
     [--node-id <N>] [--raft-port <PORT>] [--peers <LIST>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `-d`, `--database <PATH>` | (required) | Database created by `mudd_init` |
| `-b`, `--bind <ADDR>` | `127.0.0.1:8080` | HTTP and WebSocket listen address. Use `0.0.0.0:8080` to accept remote connections. |
| `--static-dir <DIR>` | (none) | Serve the built web client (for example `client/dist`) on the same origin as the API. Requests no API route claims get the matching file, falling back to `index.html` for client-side routes. Startup fails if `DIR/index.html` is missing. Without this flag, `GET /` returns `{"name":"mudd","version":"..."}`. |
| `--node-id <N>` | `1` | Raft node id |
| `--raft-port <PORT>` | `9000` | Raft port advertised to peers |
| `--peers <LIST>` | (none) | Cluster peers, `1=host1:9000,2=host2:9001`. Omit it for single-node mode. |
| `-V`, `--version` | | Print the version |

> **Clustering is not functional yet.** `--node-id`, `--raft-port` and
> `--peers` are parsed, and a node sends Raft RPCs to
> `http://<peer>/raft/...`. But no server listens for those RPCs: nothing binds
> `--raft-port`, and the HTTP router has no `/raft` routes. Run single-node,
> which is the default. See [docs/roadmap.md](../../docs/roadmap.md).

| Environment variable | Description |
|----------------------|-------------|
| `RUST_LOG` | Log filter. Default `mudd=info,tower_http=debug`. |
| `VENICE_API_KEY` | Enables Venice AI: `game.llm_chat`, `game.llm_image` and automatic room images. Without it, those features are off. |
| `VENICE_API_URL` | Venice API base URL. Default `https://api.venice.ai/api/v1`. |

#### Working directory

`POST /universe/{id}/run_script` reads files from `scripts/` **relative to the
directory `mudd` was started in**. Start `mudd` from the directory that
contains your `scripts/` folder (the repository root, for the bundled
`scripts/cave_adventure.lua`), or set `WorkingDirectory=` under systemd.

### Logging

```bash
RUST_LOG=mudd=debug mudd --database /var/lib/mudd/game.db
RUST_LOG=mudd=warn  mudd --database /var/lib/mudd/game.db
RUST_LOG=mudd::api=debug,mudd::lua=trace mudd --database /var/lib/mudd/game.db
```

## Running Directly

```bash
cd mudcroft/mudd
export MUDD_ADMIN_USERNAME=admin
export MUDD_ADMIN_PASSWORD='choose-a-long-password'
./target/release/mudd_init --database /var/lib/mudd/game.db --lib-dir lib

cd ..   # repository root, so run_script finds scripts/
./mudd/target/release/mudd --database /var/lib/mudd/game.db \
    --bind 0.0.0.0:8080 --static-dir client/dist
```

Open `http://<host>:8080/` for the web client.

For a throwaway local server with a world already loaded, use
`scripts/start-testsrv.sh` (see [playtest.md](../../playtest.md)).

### systemd

```ini
# /etc/systemd/system/mudd.service
[Unit]
Description=Mudcroft server
After=network.target

[Service]
Type=simple
User=mudd
WorkingDirectory=/usr/local/share/mudcroft
ExecStart=/usr/local/bin/mudd --database /var/lib/mudd/game.db --bind 0.0.0.0:8080 --static-dir /usr/local/share/mudcroft/www
Restart=on-failure
Environment=RUST_LOG=mudd=info
# Environment=VENICE_API_KEY=...

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now mudd
```

## Docker

The `Dockerfile` builds a single image that contains:

- `mudd` and `mudd_init` in `/app`, and the core library in `/app/lib`
- the built web client in `/var/www/html`
- **nginx** on port 80, which serves the client and proxies `/ws`, `/auth`,
  `/universe`, `/images` and `/health` to `mudd` on `127.0.0.1:8080`
- **supervisord**, which runs nginx, and `mudd` as user `appuser`

The client is served by nginx, so the container does not use `--static-dir`.

```bash
docker build -t mudcroft .
docker run -d --name mudcroft \
    -p 8080:80 \
    -v mudcroft-data:/data \
    -e MUDD_ADMIN_USERNAME=admin \
    -e MUDD_ADMIN_PASSWORD='choose-a-long-password' \
    mudcroft
```

Then open `http://localhost:8080/`.

On every start, `docker/entrypoint.sh` runs
`mudd_init --database "$MUDD_DATABASE_PATH" --lib-dir "$MUDD_LIB_DIR"`. On the
first start this creates the database. On later starts it upgrades it, and the
admin variables are no longer needed. The entrypoint then starts supervisord.

| Variable | Default | Used by |
|----------|---------|---------|
| `MUDD_DATABASE_PATH` | `/data/mudcroft.db` | entrypoint (`mudd_init`) and supervisord (`mudd --database`) |
| `MUDD_BIND_ADDR` | `127.0.0.1:8080` | supervisord (`mudd --bind`). The nginx upstream is hard-coded to `127.0.0.1:8080`, so leave this alone. |
| `MUDD_LIB_DIR` | `/app/lib` | entrypoint (`mudd_init --lib-dir`) |
| `MUDD_ADMIN_USERNAME`, `MUDD_ADMIN_PASSWORD` | (none) | entrypoint. Required on first start. |
| `RUST_LOG` | `mudd=info` | `mudd` |
| `VENICE_API_KEY`, `VENICE_API_URL` | (none) | `mudd`, inherited through supervisord |

- **Volume:** `/data` holds the database. Mount it to keep data across
  containers.
- **Ports:** only 80 (nginx) is exposed. `mudd` listens on loopback inside the
  container.
- **Health check:** the image's `HEALTHCHECK` runs `curl -f http://localhost/health`.
- **No scripts:** `mudd` runs in `/app`, and the image contains no `scripts/`
  directory, so `run_script` returns 404 unless you mount one at `/app/scripts`.

## Creating Universes

Universe creation requires an **admin** (or owner) bearer token. Log in first.
These examples use `jq`:

```bash
SERVER=http://127.0.0.1:8080
LOGIN=$(curl -s -X POST $SERVER/auth/login \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"choose-a-long-password"}')
TOKEN=$(echo "$LOGIN" | jq -r .token)
ADMIN_ID=$(echo "$LOGIN" | jq -r .account_id)
```

Each account has one session token. Logging in again, for example in the web
client, replaces it and invalidates the token above.

Universe ids are DNS-style: 3-64 characters, lowercase letters, digits and
hyphens, starting and ending with an alphanumeric character, with no `--`.
Uppercase input is lowercased. Examples: `my-game`, `test123`, `rpg-world-2`.

### From JSON

```bash
curl -s -X POST $SERVER/universe/create \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"id\":\"my-game\",\"name\":\"My World\",\"owner_id\":\"$ADMIN_ID\"}"
```

Response (201):

```json
{"id":"my-game","name":"My World","libs_loaded":["combat","commands","utils"]}
```

The universe gets every core library from `mudd_init`. An optional `libs`
object (`{"name": "<lua source>"}`) adds libraries or overrides core ones by
name. An optional `config` object is stored as-is, plus the `lib_hashes` key
the server adds.

### From a ZIP file

```
universe.zip
├── universe.json        # {"id": "...", "name": "...", "owner_id": "...", "config": {}}
└── lib/
    └── quests.lua       # every *.lua file becomes a library named after its file stem
```

```bash
curl -s -X POST $SERVER/universe/upload \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/octet-stream' \
    --data-binary @universe.zip
```

### Listing universes

```bash
curl -s $SERVER/universe/list -H "Authorization: Bearer $TOKEN"
```

Any logged-in account may list universes.

## Building and Opening a Universe

A new universe has no rooms. Until a spawn portal is set, players who connect
see `Universe not initialized. Wizards: use 'setportal' command.`

1. **Create rooms.** Either run a script file from `scripts/` (admin, or the
   universe's owner):

   ```bash
   curl -s -X POST $SERVER/universe/my-game/run_script \
       -H "Authorization: Bearer $TOKEN" \
       -H 'Content-Type: application/json' \
       -d '{"script":"cave_adventure.lua"}'
   # {"result":"Cave adventure created! Enter from: /rooms/cave-entrance"}
   ```

   Or connect as a wizard+ and build with `eval`, for example
   `eval return game.create_object("/rooms/hall", "room", nil, {name="Hall"}).id`.
   See [langtutor.md](langtutor.md).

2. **Set the portal.** As a universe admin+, over HTTP:

   ```bash
   curl -s -X POST $SERVER/universe/my-game/portal \
        -H "Authorization: Bearer $TOKEN" \
        -H 'Content-Type: application/json' \
        -d '{"room_id":"/rooms/cave-entrance"}'
   ```

   Or connect to the universe as a wizard+ and run
   `setportal /rooms/cave-entrance`. With no argument, `setportal` uses the
   room you are standing in; use `goto <room_id>` to get there first.

Wizard commands:

| Command | Description |
|---------|-------------|
| `goto <room_id>` | Teleport to a room in this universe (your player object moves too) |
| `setportal [room_id]` | Set the universe's spawn room: the given room, or the current one |
| `eval <lua>` | Run Lua as yourself, with the larger eval budget |

Both `goto` and `setportal` only see rooms in the current universe.

The portal is stored in `universe_settings` under the key `portal_room_id`:

```sql
SELECT value FROM universe_settings WHERE universe_id = 'my-game' AND key = 'portal_room_id';
```

## Accounts and Access Levels

Anyone can register:

```bash
curl -s -X POST $SERVER/auth/register \
    -H 'Content-Type: application/json' \
    -d '{"username":"player1","password":"password123"}'
```

Usernames must be 3-32 ASCII letters, digits, `_` or `-`, and passwords at
least 6 characters; anything else gets HTTP 400. New accounts are `player`.
For what each level may do, and how to change
levels (including creating the first `owner`, and how levels are cached), see
[permissions.md](permissions.md). The short version: as an admin, connect
and run

```
eval return game.set_access_level("<account_id>", "wizard")
```

The promoted player must reconnect before wizard commands work.

## Lua Execution Limits

| Execution | Instructions | Wall time | DB calls | Venice calls | Memory |
|-----------|--------------|-----------|----------|--------------|--------|
| `eval`, `run_script`, `get`/`drop`/`inventory` | 10,000,000 | 5 s | 10,000 | 5 | 64 MB |
| Object code: hooks, verbs, timers, `use_object` | 1,000,000 | 500 ms | 100 | 5 | 64 MB |

Exceeding a limit aborts the script with an error such as
`instruction limit exceeded (1000001 > 1000000)`.

## Monitoring

```bash
curl -s http://127.0.0.1:8080/health
# {"status":"healthy","database":"ok"}       HTTP 200
# {"status":"unhealthy","database":"error"}  HTTP 503
```

`GET /` returns `{"name":"mudd","version":"0.1.0"}`, but only when
`--static-dir` is not set. With it, `/` serves the client.

## Backup and Restore

The database uses WAL mode. Either use SQLite's online backup:

```bash
sqlite3 /var/lib/mudd/game.db ".backup /backup/game.db"
```

Or stop the server and copy all three files:

```bash
sudo systemctl stop mudd
cp /var/lib/mudd/game.db /var/lib/mudd/game.db-wal /var/lib/mudd/game.db-shm /backup/ 2>/dev/null
sudo systemctl start mudd
```

To restore a `.backup` copy:

```bash
sudo systemctl stop mudd
cp /backup/game.db /var/lib/mudd/game.db
rm -f /var/lib/mudd/game.db-wal /var/lib/mudd/game.db-shm
sudo systemctl start mudd
```

## Upgrading

```bash
sudo systemctl stop mudd
# install the new binaries and lib/ files, then:
mudd_init --database /var/lib/mudd/game.db --lib-dir /usr/local/share/mudcroft/lib
sudo systemctl start mudd
```

`mudd_init` runs the schema migrations and refreshes `core_lib_hashes`.
Accounts and game data are kept. `mudd` itself can be stopped and started
again on the same database at any time: it resumes the persisted Raft state
(vote, log, membership) instead of initializing a new cluster, and adds newer
Raft columns to older databases on startup. Existing universes keep their library
versions (see [mudd_init](#mudd_init)).

## Troubleshooting

| Message | Cause and fix |
|---------|---------------|
| `Database file not found: <path>. Run mudd_init to create it.` | `mudd` never creates a database. Run `mudd_init` first, or check the path. |
| `MUDD_ADMIN_USERNAME environment variable required for new database` | Set `MUDD_ADMIN_USERNAME` and `MUDD_ADMIN_PASSWORD` when creating a database. They are not needed for upgrades. |
| `Admin password must be at least 8 characters` | Use a longer `MUDD_ADMIN_PASSWORD`. |
| `Database is missing required table '<name>'` | Run `mudd_init` on the database to migrate it. |
| `Database schema version N does not match this mudd (M). Run mudd_init to upgrade it.` | The database was made by a different version. Run `mudd_init` on it. |
| `Database schema version N predates version M and there is no upgrade from it; create a new database` | (from `mudd_init`) No upgrade path exists from that version. Create a new database. |
| `Database has no admin account. Run mudd_init to create a valid database.` | No account has `access_level` `admin` or `owner`. `mudd_init` will not add one to an existing database. Run `mudd_admin --database <PATH> set-level <USERNAME> admin`. |
| `--static-dir <dir> has no index.html (build the client first)` | Run `npm run build` in `client/`, or fix the path. |
| `Address already in use` | Another process holds the `--bind` port. Pick another address or stop that process. |
| `get`/`drop`/`inventory` reply with a Lua error about `Commands` | The universe has no core library. It was created after a `mudd_init` that did not find `lib/`. Re-run `mudd_init` with `--lib-dir`, then create a new universe. |
| `Universe not initialized. Wizards: use 'setportal' command.` | No portal is set. See [Building and Opening a Universe](#building-and-opening-a-universe). |
| `Script not found: <name> (...)` from `run_script` | `mudd` was started outside the directory that contains `scripts/`. See [Working directory](#working-directory). |
| `Permission denied: wizard+ required for eval` | Promote the account, then reconnect. |
| `instruction limit exceeded (...)` | The script hit its budget. See [Lua Execution Limits](#lua-execution-limits). |

## Useful SQL

```sql
-- Accounts
SELECT id, username, access_level, created_at FROM accounts;

-- Universes
SELECT id, name, owner_id, created_at FROM universes;

-- Objects per universe
SELECT universe_id, COUNT(*) FROM objects GROUP BY universe_id;

-- Path grants
SELECT universe_id, grantee_id, path_prefix, can_delegate FROM path_grants;

-- Pending call_out timers
SELECT id, object_id, method, fire_at FROM timers ORDER BY fire_at;

-- Credit balances
SELECT player_id, balance FROM credits WHERE universe_id = 'my-game';

-- Universe settings, including the portal
SELECT * FROM universe_settings WHERE universe_id = 'my-game';
```

Changes made with SQL bypass Raft and the server's in-memory caches. Make them
with the server stopped.
