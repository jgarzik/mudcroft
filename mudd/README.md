# HemiMUD

Multi-user dungeon server with Lua scripting, Raft replication, and AI integration.

## Features

- Sandboxed Lua scripting with instruction/memory metering
- LPC-style object system with class inheritance
- D&D-style combat with dice rolling, damage types, status effects
- Role-based permissions (player, builder, wizard, admin, owner)
- Timers: one-shot `call_out` and recurring `heartbeat`
- WebSocket multiplayer with JWT authentication
- Raft consensus for cluster replication
- Venice AI integration (LLM chat, image generation)
- SQLite persistence with WAL mode

## Quick Start

```bash
# 1. Initialize database
export MUDD_ADMIN_USERNAME=admin
export MUDD_ADMIN_PASSWORD=secretpass123
mudd_init --database /path/to/game.db

# 2. Start server
mudd --database /path/to/game.db --bind 127.0.0.1:8080

# 3. Connect via WebSocket
wscat -c "ws://127.0.0.1:8080/ws?token=<jwt_token>"
```

## CLI Reference

### mudd_init

One-time database initialization.

```
mudd_init --database <path> [--lib <file.lua>...]

Options:
  -d, --database <PATH>   SQLite database path (must not exist)
  --lib <PATH>            Lua library file to store (repeatable)

Environment:
  MUDD_ADMIN_USERNAME     Admin account username (required)
  MUDD_ADMIN_PASSWORD     Admin account password (required, min 8 chars)
```

### mudd

Server daemon.

```
mudd --database <path> [--bind <addr>]

Options:
  -d, --database <PATH>   Pre-initialized SQLite database (required)
  -b, --bind <ADDR>       Listen address [default: 127.0.0.1:8080]

Environment:
  RUST_LOG                Log level filter [default: mudd=info,tower_http=debug]
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `RUST_LOG` | Log level filter | `mudd=info,tower_http=debug` |
| `MUDD_ADMIN_USERNAME` | Admin username (init only) | required |
| `MUDD_ADMIN_PASSWORD` | Admin password (init only) | required |

## Building from Source

```bash
cd mudd
cargo build --release

# Binaries at:
#   target/release/mudd
#   target/release/mudd_init
```

### Running Tests

```bash
cargo test --lib           # Unit tests (fast)
cargo test --release       # All tests including integration
cargo clippy --all-targets # Lint check
```

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Server info (name, version) |
| `/health` | GET | Health check (database status) |
| `/ws` | GET | WebSocket upgrade (token query param) |
| `/auth/register` | POST | Create account |
| `/auth/login` | POST | Login, get token |
| `/auth/logout` | POST | Invalidate token |
| `/auth/validate` | GET | Validate token |
| `/universe/create` | POST | Create universe |
| `/universe/upload` | POST | Create universe from ZIP |
| `/images/:hash` | GET | Retrieve stored image |

## License

See LICENSE file in repository root.
