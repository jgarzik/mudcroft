# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

HemiMUD is a multi-user dungeon (MUD) game server in Rust featuring:
- Sandboxed Lua scripting for programmable game mechanics
- Raft-based distributed consensus for cluster replication
- WebSocket-based multiplayer gameplay
- LPC-style object system with class inheritance

## Build & Test Commands

All commands run from the `mudd/` directory:

```bash
cargo build --release          # Build
cargo test --lib               # Unit tests only (fast)
cargo test --release                    # All tests including integration
cargo test --release test_name           # Run single test
cargo clippy --all-targets           # Lint check
cargo run -- --bind 127.0.0.1:8080 --database /tmp/mudcroft.db
```

## Architecture

### Core Components (mudd/src/)

- **api/** - HTTP/WebSocket endpoints via Axum
  - `auth.rs` - /auth/* endpoints (register, login, logout, validate)
  - `websocket.rs` - /ws endpoint, ConnectionManager, PlayerSession
- **objects/** - LPC-style object system
  - `store.rs` - ObjectStore CRUD operations
  - `class.rs` - ClassDef, ClassRegistry, inheritance
- **lua/** - Lua sandbox with metering
  - `sandbox.rs` - Execution with instruction/memory limits
  - `game_api.rs` - Exposed game.* functions
  - `actions.rs` - ActionRegistry for contextual verbs
- **combat/** - Combat system with damage types, effects, PvP policies
- **raft/** - OpenRaft consensus for multi-node replication
- **timers/** - call_out (one-shot) and heartbeat (recurring) callbacks
- **permissions/** - Role-based access (player < builder < wizard < admin < owner)
- **credits/** - Economy system
- **venice/** - AI integration (LLM chat, image generation)

### Database

SQLite with tables: accounts, universes, objects, code_store, permissions, combat_state, timers, heartbeats, credits, raft_logs, raft_votes, snapshots

### WebSocket Protocol

- Connect with universe and optional auth: `/ws?universe=<id>&token=...`
- Universe ID required (DNS-style: 3-64 chars, lowercase alphanumeric and hyphens)
- Commands: look, north/south/east/west, say, help
- Messages: Server pushes room descriptions, messages, player status

## Test Infrastructure

The `tests/harness/` directory provides:
- **TestServer** - Spawns actual mudd binary as subprocess
- **TestClient** - Role-based authenticated WebSocket client
- **TestWorld** - Pre-configured universe with regions/rooms

## Key Design Documents

- `design.md` - Full architectural specification
- `plan.md` - Implementation phases 1-13
- `igor-history.md` - LP-MUD theory and historical context
