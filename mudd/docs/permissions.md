# Permission System Reference

Mudcroft uses an LPMud-style permission model. The Rust server enforces it,
and Lua code cannot bypass it. It combines three things:

1. **Access level**: a role per universe (`universe_members`), plus global
   admin/owner accounts (`accounts.access_level`).
2. **Path grants**: per-universe delegation of an object-path prefix such as `/d/forest`.
3. **Ownership**: every object records the account that created it (`objects.owner_id`).

This page is the canonical reference for access levels. Other documents link here.

## Access Levels

Levels are ordered. Each level has everything the levels below it have.

Levels are **per universe**. An account's effective level in a universe is
the highest of:

- its member level there (`universe_members`; no row means `player`)
- its global level (`accounts.access_level`), but only if that is `admin` or
  `owner`: global admins and owners have their level in every universe, and
  lower global values mean nothing
- `owner`, if it owns the universe (`universes.owner_id`)

So a wizard of one universe is a plain player in another, and a universe's
owner has owner power inside that universe only.

Acting as another account (`game.set_actor`, `run_script` with
`account_id`, attaching code to or wiring verbs on objects it owns) requires
outranking it: a strictly higher effective level in the universe, and, if the
target is a global admin or owner, a strictly higher global level as well
(`PermissionManager::outranks`). A universe's owner therefore never gains a
global admin's identity.

| Level | Rank | Adds |
|-------|------|------|
| `player` | 0 | The default for new accounts. Play: look, move, `say`, take and drop objects that are neither `fixed` nor living, attack NPCs. Can read objects and modify objects it owns. |
| `builder` | 1 | The `create` command. Can create, modify, move and delete objects under its path grants. |
| `wizard` | 2 | Bypasses object checks: can create, modify, move or delete any object. Can use the `eval`, `goto` and `setportal` commands, `game.store_code`, `game.define_class`, `game.set_time` and `game.set_rng_seed`. Can grant any path. Can `game.set_actor` to accounts below wizard. |
| `admin` | 3 | `run_script` on the universe. `game.admin_grant_credits` and `game.update_universe`. `game.set_access_level` on accounts below admin, to levels below admin. A **global** admin can also `POST /universe/create` and `/universe/upload`. |
| `owner` | 4 | `game.set_access_level` on any account, including granting or revoking `admin` membership. |

`mudd_init` creates the first account as a global `admin`. Other global
levels are set with `mudd_admin` (see [Changing access levels](#changing-access-levels)).

## Who a Script Runs As

Every Lua execution has an acting account. Permission checks, and the owner of
any object the script creates, are based on that account.

| Execution | Runs as |
|-----------|---------|
| `eval` (WebSocket) | The wizard who typed it |
| `get`/`take`, `drop`, `inventory` | The player |
| `POST /universe/{id}/run_script` | The authenticated caller, or `account_id` (see [API.md](API.md#post-universeidrun_script)) |
| Object code: hooks (`on_enter`, `on_leave`, `on_damage`, `on_death`), custom verbs (`game.add_action`), `call_out` and heartbeat callbacks, `game.use_object` | **The object's owner**, never the player who triggered it |

Object code runs as the owner so that code planted by a low-privilege author
cannot borrow the privileges of whoever walks into it. An object with no owner
runs with no account, and every permission-checked write it attempts fails.

`game.set_actor(account_id)` switches the acting account for the rest of the
execution. Only a wizard+ may call it, and only to an account strictly below
their own level (`AccessLevel::can_act_as`). `game.set_actor(nil)` clears it.
It returns `true` or `false`.

## Permission Check Algorithm

`PermissionManager::check_permission` takes an acting account, an action and a
target object. The first matching rule decides:

1. **Admin-only and wizard-only actions** are decided here, before any bypass:
   - `admin_config` and `grant_credits` are allowed for admin+ and denied
     for everyone else, wizards included.
   - `store_code` is allowed for wizard+ and denied for everyone else.
2. **Wizard bypass**: an account at wizard or above is allowed.
3. **Ownership**: allowed if `object.owner_id` equals the acting account.
4. **Path grant**: allowed if any of the account's grants in this universe
   covers the object id (see [Path matching](#path-matching)).
5. **Player defaults**: `read` and `execute` are allowed. `move` is allowed
   unless the object has the property `fixed = true`.
6. Everything else is denied.

Creating an object uses a separate check, `check_create_permission`:
allowed for wizard+, otherwise only if a path grant covers the new object's path.

Object ids are scoped per universe, and a script reads and writes only its
own universe's objects, whatever the acting account's level. Scripts cannot
create player-class objects or anything under `/players/`.

## Actions

| Action | Who may | Enforced by |
|--------|---------|-------------|
| `read` | Everyone | Only via `game.check_permission`. `game.get_object` does not check it. |
| `execute` | Everyone | Only via `game.check_permission` |
| `move` | Anyone if the object is not `fixed`. Owner, path grant or wizard+ otherwise. | `game.move_object` (the object's own path only) |
| `modify` | Owner, path grant, wizard+ | `game.update_object` |
| `delete` | Owner, path grant, wizard+ | `game.delete_object` |
| `create` | Path grant on the new path, or wizard+ | `game.create_object`, `game.clone_object`, the `create` command |
| `store_code` | Wizard+ | `game.store_code`, `game.define_class` |
| `admin_config` | Admin+ | `game.update_universe` |
| `grant_credits` | Admin+ | `game.admin_grant_credits` |

### Attaching code

Setting `code_hash` with `game.update_object` makes code run as the object's
owner. So in addition to `modify`, the acting account must be the owner or
outrank the owner (`can_act_as`). Otherwise the call fails with "only the owner
or a superior may attach code".

A `code_hash` passed to `game.create_object` attaches to the new object, which
the acting account owns. The hash must already exist in `code_store`. A
class's `code_hash` default is never attached to instances; only an explicit
`code_hash` attaches code.

### Wiring verbs

A custom verb's handler runs as the object's owner, so `game.add_action` and
`game.remove_action` follow the same rule as attaching code: `modify` on the
object, and the acting account must be the owner or outrank the owner
(`can_act_as`). Ownerless objects need only `modify`. `add_action` also
refuses private methods (names starting with `_`, and the engine hooks
`on_enter`, `on_leave`, `on_damage`, `on_death`, `heart_beat`, `on_create`,
`on_destroy`), and verb dispatch never invokes one.

### Calling other objects

`game.use_object` runs the target's handler as the target's owner, whoever
calls it. To keep that from lending the owner's rights to arbitrary callers:

- private handlers (`_name`) and engine hooks are never callable through it;
  such calls return `nil`
- one top-level execution may make at most 32 `use_object` calls across all
  nesting levels, and nesting stops at 8 levels
- the `actor_id` argument is supplied by the caller, so handlers must treat it
  as untrusted rather than as proof of which player acted

### Classes

`game.define_class` requires wizard+. A defined class belongs to the
script's universe (the built-in classes are global). The built-in base classes cannot be redefined, and an existing
custom class may be redefined only by the account that defined it, or by an
admin+ (`ClassDef::defined_by`).

## Path Grants

A path grant lets an account work on part of the object tree in one universe.

### Path matching

Grants match on `/` boundaries (`PathGrant::covers_path`), not by plain string
prefix. A grant for `/d/forest` covers:

- `/d/forest` (exact match)
- `/d/forest/cave`
- `/d/forest/rooms/clearing`

It does not cover:

- `/d/forestville` (shares the prefix, but not on a `/` boundary)
- `/d/other/forest`

### Grant fields

| Field | Description |
|-------|-------------|
| `id` | Grant id (UUID) |
| `universe_id` | Universe the grant applies to |
| `grantee_id` | Account receiving the grant |
| `path_prefix` | Path covered, e.g. `/d/forest` |
| `can_delegate` | Whether the grantee may grant sub-paths to others |
| `granted_by` | Account that created the grant |
| `granted_at` | RFC 3339 timestamp |

Grants are stored in the `path_grants` table ([SCHEMA.md](../../SCHEMA.md#path_grants)).

### Delegation and revocation

- A wizard+ may grant any path, with or without `can_delegate`.
- Anyone else may grant only a path covered by one of their own grants that has
  `can_delegate = true`.
- A grant may be revoked by a wizard+, by the account that created it, or by
  anyone who could delegate its path.

## Lua API

All functions act in the current universe as the acting account.

```lua
-- Check an action against a target. Returns {allowed = bool, error = string?}.
-- Unknown action names return allowed = false.
local r = game.check_permission("modify", "/d/forest/cave", false, owner_id)
if not r.allowed then
    return "Denied: " .. r.error
end

-- True if the acting account is wizard+ or has a grant covering the path
if game.can_access_path("/d/forest") then
    -- ...
end
```

### Access levels

```lua
local level = game.get_access_level(account_id)  -- "player", "builder", ...

-- Returns true if changed, false if refused, the level name is unknown or
-- the write failed
local ok = game.set_access_level(account_id, "wizard")
```

Both act on the script's universe: `get_access_level` returns the effective
level there, and `set_access_level` writes the account's member level there.
`game.set_access_level` follows `AccessLevel::can_set_access_level`, using
effective levels:

- An owner may set any account to `player`, `builder`, `wizard` or `admin`.
- An admin may change only accounts strictly below admin, and only to
  `player`, `builder` or `wizard`.
- Anyone below admin is refused.
- `owner` is never a member level; it comes from owning the universe or from
  a global owner account.

### Path grants

```lua
-- Returns the grant table, or {error = "..."}
local g = game.grant_path(grantee_id, "/d/forest", true)  -- true = can_delegate
if g.error then
    return "Failed: " .. g.error
end

-- Returns true if revoked, false if not found, or {error = "..."} if refused
local revoked = game.revoke_path(g.id)

-- Every grant held by an account in this universe
for _, grant in ipairs(game.get_path_grants(account_id)) do
    print(grant.path_prefix, grant.can_delegate)
end
```

## Changing Access Levels

Prefer `game.set_access_level` from an `eval` session in the universe. For
example, an admin promoting a player to wizard in that universe:

```
eval return game.set_access_level("<account_id>", "wizard")
```

Global levels have no Lua or HTTP path. To make an account a global admin or
owner, or remove its global level, stop the server and use `mudd_admin`:

```
mudd_admin --database /var/lib/mudd/game.db set-level alice owner   # or admin, none
```

It refuses to remove the last global admin or owner: the server will not
start without one (see [admin.md](admin.md#troubleshooting)).

### Caching

Different checks read the level at different times:

- **HTTP endpoints**: `create` and `upload` read the global level from the
  database on every request; `run_script` uses `PermissionManager`.
- **WebSocket commands** (`eval`, `goto`, `setportal`, `create`, role-aware
  `help`) read the level from `PermissionManager` for every command, so a
  promotion or demotion applies to open sessions at once.
- **Lua permission checks** go through `PermissionManager`. It caches global
  levels, member levels and universe owners in memory after the first
  lookup, and only `game.set_access_level` updates that cache. SQL run by
  hand against a live server may not take effect until `mudd` restarts.

Path grants are also cached in memory. They are loaded at startup and updated
by `grant_path` and `revoke_path`. Do not edit the `path_grants` table while
the server runs.

## Security Notes

- Enforcement is in Rust. Lua sees only the `game.*` functions, and each
  checks the acting account.
- Path grants match on `/` boundaries, so `/d/forest` never leaks into
  `/d/forestville`.
- Grants record who created them and when.
- Object code never runs with the privileges of the player who triggered it.
