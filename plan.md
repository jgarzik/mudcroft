# HemiMUD Implementation Progress

## Phase 1: Foundation + Test Harness - COMPLETE
- Cargo workspace, SQLite, Axum, MuddTest harness
- 7 tests (2 unit, 5 integration)

## Phase 2: Lua Sandbox - COMPLETE
- mlua sandboxing, instruction limits, memory limits, metering
- 12 tests

## Phase 3: Object System - COMPLETE
- Object CRUD, ClassRegistry, inheritance, code store, game API stubs
- 10 tests

## Phase 4: Rooms & Movement - COMPLETE
- Room exits (north/south/east/west)
- get_environment, get_living_in, get_exit, set_exit, remove_exit
- Player movement between rooms
- 4 tests

## Phase 5: WebSocket & Commands - COMPLETE
- WebSocket upgrade endpoint at /ws
- ConnectionManager for session tracking
- Command parser (look, north/south/east/west, say, help)
- ActionRegistry for contextual verbs (game.add_action, remove_action)
- MessageQueue for messaging (game.send, broadcast, broadcast_region)
- WsClient test helper for WebSocket integration tests
- 9 tests (5 WebSocket integration, 4 unit tests)

## Phase 6: Permissions & Access Control - COMPLETE
- AccessLevel enum: player, builder, wizard, admin, owner
- PermissionManager for user levels and region assignments
- game.check_permission(action, target_id, is_fixed, region_id)
- game.get_access_level, set_access_level
- game.assign_region, unassign_region for builders
- Fixed object enforcement (players can't move fixed objects)
- Wizard bypass for fixed objects
- 8 tests

## Phase 7: Combat System - COMPLETE
- DiceRoll parser and roller (2d6+3, 1d20, etc.)
- DamageType enum (physical, fire, cold, poison, etc.)
- DamageProfile with immunity/resistance/vulnerability
- StatusEffect system (poisoned, stunned, burning, etc.)
- EffectRegistry for managing effects per entity
- CombatState tracking (in_combat, attackers, hp, etc.)
- CombatManager for combat operations (initiate, attack, deal_damage)
- PvpPolicy enum (disabled, arena_only, flagged, open)
- Critical hits (nat 20) and fumbles (nat 1)
- 31 tests

### Current Stats
- **Total tests:** 110 (91 unit, 19 integration)
- **Clippy:** clean (minor dead_code warnings for not-yet-integrated APIs)

---

## Phase 8: Timers & Delayed Execution - COMPLETE
- Timer struct for one-shot delayed callbacks
- HeartBeat struct for recurring callbacks
- TimerManager with in-memory + DB persistence
- game.call_out(delay_secs, method, args) - returns timer_id
- game.remove_call_out(timer_id)
- game.set_heart_beat(interval_ms)
- game.remove_heart_beat()
- tick() method returns due callbacks for execution
- 7 tests

---

## Phase 9: Credit System - COMPLETE
- CreditManager with in-memory + DB persistence
- game.get_credits() - current player's balance
- game.deduct_credits(amount, reason) - returns bool
- game.admin_grant_credits(account_id, amount) - wizard+ only
- 7 tests

---

## Phase 10: Venice AI Integration - COMPLETE
- VeniceClient for OpenAI-compatible API
- RateLimiter with token bucket (60 req/min)
- ModelTier: fast, balanced, quality
- ImageStyle: realistic, anime, digital, painterly
- game.llm_chat(messages, tier) - returns response text
- game.llm_image(prompt, style, size) - returns URL
- 7 tests

---

## Phase 11: Raft Replication - COMPLETE
- RaftStorage with SQLite persistence (logs, votes, metadata)
- RaftNetwork for node communication
- SnapshotManager for state snapshots
- GameStateMachine for SQL replication
- CombinedStorage adaptor for OpenRaft 0.9
- 31 tests

---

## Phase 12: Authentication - COMPLETE
- Token generation with SHA-256
- Password hashing with per-user salt
- AccountService for CRUD operations
- HTTP endpoints: /auth/register, /auth/login, /auth/logout, /auth/validate
- WebSocket auth via ?token= query parameter
- 13 tests

---

## Phase 13: Integration Test Harness - COMPLETE
- `tests/harness/` directory with TestServer, TestClient, TestWorld
- **True end-to-end testing**: spawns actual `mudd` binary as subprocess
- CLI argument parsing: `--bind <addr>` and `--database <path>`
- On-disk SQLite temp DB per test (real filesystem, max code coverage)
- Role-based auth: Guest/Player/Builder/Wizard/Admin
- TestWorld auto-setup with universe, region, rooms, accounts
- Race condition tests using tokio::join!
- 160 tests (135 unit + 25 integration)
