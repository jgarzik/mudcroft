# Multi-node Mudcroft: cluster design

Status: **reviewed** (Phase 5a; decisions recorded below). Nothing here is built yet.

## Goals and decisions already made

- Raft is always on; a single node is just a one-member cluster (today).
- One **primary** (the Raft leader) runs all Lua, timers, heartbeats and the
  effect scheduler, and executes every player command.
- Other nodes are **gateways**: they hold players' WebSocket connections and
  forward commands to the primary, and its output back.
- The cluster port is protected by **TLS plus a shared secret**.
- The tables, written only through Raft, are the source of truth; in-memory
  maps are caches (see `CLAUDE.md`).

Non-goals for now: sharding universes across primaries, multi-region,
reads served by gateways for gameplay.

## Why single-primary keeps this simple

Every write originates on the primary, and the primary applies its own writes
before `client_write` returns. So as long as a new primary has applied
everything its predecessors committed **before** it executes anything, all
game logic sees consistent state, exactly as on a single node. That gives:

- **No cache invalidation on apply.** Caches (classes, access levels, effects,
  timers, verbs) are used only on the primary. A node builds them from the
  tables when it becomes primary and drops them when it stops being primary.
  This replaces the roadmap item "invalidate per-node caches on apply".
- **No write forwarding.** `RaftWriter` on a non-primary returns a
  `NotPrimary` error; nothing on a gateway writes. (roodb makes the same
  choice: secondaries are read-only.)
- **No distributed locks.** The effect write lock, image jobs and so on stay
  process-local, because only one process runs them.

## Node roles and lifecycle

Each node watches `raft.metrics()` for `current_leader`.

**Becoming primary** (in order):

1. `raft.ensure_linearizable(ReadPolicy::ReadIndex)`: confirms leadership
   with a quorum and waits until this node has applied everything committed
   before its term.
2. Rebuild every cache from its table: classes, access levels, status
   effects, timers and heartbeats, custom verbs.
3. Start the timer loop and the effect scheduler. Timers and effects use
   absolute deadlines, so anything due during the failover fires at once,
   and only once: the old primary's writes either committed (and the new
   primary sees them) or they did not.
4. Accept gateway streams and local game connections.

**Losing primary** (a newer leader, or a lost quorum): stop the schedulers
(abort their tasks), close every game session and gateway stream (they
reconnect to the new primary), drop the caches. An old primary that has not
noticed yet cannot do damage: its writes fail at `client_write`. It may at
most send a few stale messages to players for up to one election timeout.

**Known limitation until Phase 6:** custom verbs (`ActionRegistry`) live
only in memory, so a failover loses them (the new primary has none until
object code re-registers them). Persisting them stays in Phase 6.

## Gateways

Players may connect to any node. On a gateway:

- **WebSocket:** for each player the gateway multiplexes a session over one
  long-lived stream to the primary:
  - gateway to primary: `SessionOpen { session, universe, token }`,
    `Command { session, text }`, `SessionClose { session }`
  - primary to gateway: `Deliver { session, ServerMessage }`,
    `SessionEnded { session, reason }`

  On the primary, `ConnectionManager` sessions get a sink that is either a
  local WebSocket or a remote session (gateway stream + session id). All game
  code is unchanged: `send_to_player` and room broadcasts just write to the
  sink. The primary validates the token in `SessionOpen`, as `/ws` does
  today.
- **HTTP API** (`/auth/*`, `/universe/*`): forwarded to the primary over the
  cluster port (method, path, `Authorization`, body; the response comes back
  as is). A gateway never answers these from its own possibly stale copy.
- **Served locally:** the web client (`--static-dir`), `/images/{hash}`
  (content-addressed and replicated, so never stale), and `/health`, which
  reports the role and the known primary.

**Failover as seen by a player:** the gateway's stream drops; the gateway
waits for the new leader in its metrics, reconnects, and re-opens each
session. The player gets a notice and a fresh `room` message. Players
connected directly to the old primary reconnect themselves (the client
already reconnects unless it was `replaced`). Target: under 1 s of
disruption (election 150–300 ms plus reconnect).

**In-flight commands are not retried.** A command whose primary died may or
may not have taken effect, and today one command can make several Raft
writes, so a retry is not safe. The player is told the command was
interrupted. Exactly-once commands need each Lua run's writes buffered into
one batch (also wanted for atomicity) plus a command id recorded in that
batch; that is future work.

## Cluster transport

One TLS listener per node on `--cluster-port`, carrying Raft RPCs, gateway
streams and forwarded HTTP. Following roodb, which has run this in
production-like tests:

- Raw TCP inside TLS (`tokio-rustls`), frames
  `[type u8][len u32 BE][bincode body]`, `TCP_NODELAY`, and each frame sent
  as one write (roodb saw delayed-ACK stalls longer than the RPC timeout with
  two writes).
- One persistent connection per peer for Raft RPCs, reconnect and retry once
  on a failed reused connection; every RPC wrapped in a timeout from
  `option.soft_ttl()` (openraft leaves timeouts to the transport).
- Snapshots on a dedicated connection, streamed in 1 MiB chunks with a stall
  timeout, cancellable.
- Bodies are JSON rather than bincode: log entries carry
  `serde_json::Value` parameters, which bincode cannot decode. (Built:
  `raft/transport.rs`.)

**Security:**

- Every node has a certificate signed by a cluster CA
  (`--cluster-cert`, `--cluster-key`, `--cluster-ca`). Clients verify the
  server against the CA. Operators supply the CA, certificates and secret;
  `admin.md` documents the `openssl` commands (mudd generates none).
- After the TLS handshake, the connecting side sends
  `Hello { node_id, secret }`. The secret comes from `--cluster-secret-file`
  and is compared in constant time; the node id must be a cluster member (or
  a joining node, see Membership). Anything else closes the connection.
- The public port (players, HTTP) is unchanged.

## Snapshots

Today's snapshot is unsafe for a cluster:

- It reads the database file while writes continue (not consistent).
- It includes the leader's own `raft_log`, `raft_vote` and `raft_meta`, which
  would overwrite the receiver's Raft state.
- Installing it closes and replaces the pool the whole application reads
  through.

Proposed:

- **Build:** `VACUUM INTO '<tmp>'` (one consistent read transaction while
  writes continue), then drop the `raft_*` tables from the copy. The snapshot
  position is the `last_applied` stored *inside* the copy, not the in-memory
  value, because openraft builds snapshots while applying (roodb learned
  this too).
- **Send:** stream the file in chunks (above).
- **Install:** receive into a temp file, then in **one write transaction on
  the live database**: `ATTACH` the file, `PRAGMA defer_foreign_keys = ON`,
  and for each game table `DELETE FROM t; INSERT INTO t SELECT * FROM
  snap.t`, write `last_applied` and the membership, `COMMIT`, `DETACH`. The
  node has the old state or the new, never a mix; no pool is replaced; open
  connections stay valid. A node only installs snapshots as a non-primary,
  so no caches need rebuilding.
- **Policy:** `SnapshotPolicy::Never`, plus a task that calls
  `raft.trigger().snapshot()` when the log exceeds a size budget, then purges
  the log up to the snapshot (roodb does this; it keeps snapshots rare and
  the log bounded).

## Membership

Recommended: roodb's model rather than a static `--peers` list.

- **Bootstrap:** `mudd_init` creates node 1 as a one-member cluster (as
  today).
- **Join:** a new node starts with an empty database and
  `--join host:port[,...]`. Any member that is not the leader answers with
  the leader's address. The leader assigns the node id through the log, adds
  it as a learner, and promotes it to voter once it has caught up. Ticks,
  heartbeats and elections are switched on (`runtime_config()`) when the
  second member arrives.
- **Remove:** `mudd_admin cluster remove <node>` (talks to the leader over
  the cluster port with the secret); refuses to remove the last voter. A
  removed node refuses to start again; a wiped node rejoins only with a new
  id.
- **Inspect:** `mudd_admin cluster status` (members, roles, leader, log and
  apply positions).
- Each node needs `--advertise` (an address peers can reach; `0.0.0.0` is
  refused).

## Durability (with Phase 6)

Phase 6's single durable commit (only the Raft log fsyncs; the state machine
applies with `synchronous = NORMAL`) interacts with snapshots and purge: the
log may only be purged up to a position SQLite has made durable, i.e.
`last_applied` as of the last WAL checkpoint. The purge task above must use
that position, not the in-memory one.

## Operational notes

- Nodes' clocks must be synchronized (NTP): timers and effects use absolute
  wall-clock deadlines, and a new primary with a skewed clock fires them
  early or late.
- Every node needs the Venice settings, since any node may become primary.
- Losing a quorum means no primary: players get a "world unavailable" notice
  until a majority is back.
- Backups: Raft is not a backup. `VACUUM INTO` from any node (it is
  consistent while running) to offsite storage.

## Test plan

A cluster harness starting several `mudd` processes on random ports, with
certificates generated at test time (`rcgen` as a dev-dependency):

- A 3-node cluster forms by `--join`; writes replicate.
- A player on a gateway plays normally (move, say, take, attack, verbs), and
  hears a player connected to the primary.
- Kill the primary: players on gateways continue within 2 s, see the notice
  and a room message; no committed data lost; a heartbeat and a call_out due
  during the failover each fire exactly once.
- A late joiner receives a snapshot while writes continue, then follows the
  log; its database matches the leader's.
- A wrong secret, an unknown CA and a removed node are refused.
- The single-node mode behaves exactly as today (the existing suites).

## Implementation phases (5b)

1. Cluster transport: TLS listener, `Hello` with the secret, framed Raft
   RPCs, `--join`, learner then voter, `mudd_admin cluster status/remove`.
2. Snapshots: `VACUUM INTO` build, streaming, ATTACH-and-copy install,
   size-triggered snapshot and purge.
3. Primary lifecycle: `ensure_linearizable`, cache rebuild, primary-only
   schedulers, stop on losing leadership, `NotPrimary` writes.
4. Gateways: session multiplexing, forwarded HTTP, reconnection on failover.
5. Failover tests and docs (`admin.md` cluster section, `design.md`).

## Decisions (review, 2026-11-22)

1. **Membership:** `--join` with learner-then-voter promotion; no static
   `--peers` list.
2. **Peer authentication:** shared secret over server-verified TLS. The CA,
   certificates and secret are supplied by the operator.
3. **In-flight commands at failover:** reported as interrupted, never
   retried.
4. **Custom verbs:** persisted in Phase 6, after the cluster work; lost on
   failover until then.
