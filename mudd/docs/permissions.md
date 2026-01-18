# Permission System Reference

HemiMUD implements an LPMud-style path-based permission system with mandatory Rust enforcement.

## Overview

Permissions are based on three concepts:
1. **Access Levels** - Global role (Player, Builder, Wizard, Admin, Owner)
2. **Path Grants** - Delegated access to specific object path prefixes
3. **Ownership** - Creators can always modify their own objects

## Access Levels

| Level | Description |
|-------|-------------|
| `player` | Normal player, can interact with non-fixed objects |
| `builder` | Can create/modify objects under granted path prefixes |
| `wizard` | Can modify any object, like UNIX root |
| `admin` | Full universe administration |
| `owner` | Universe owner, can grant admin access |

## Permission Check Algorithm

Permission checks follow this order (first match wins):

1. **Wizard+ bypass**: `access_level >= Wizard` → Allowed
2. **Owner check**: `object.owner_id == user.account_id` → Allowed
3. **Path grant**: Any grant where `object.id.starts_with(grant.path_prefix)` → Allowed
4. **Player actions**: Read/Execute/Move-non-fixed → Allowed
5. **Default**: Denied

## Path Grants

Path grants allow builders to work in specific areas of the game world.

### How Path Matching Works

A grant for `/d/forest` covers:
- `/d/forest` (exact match)
- `/d/forest/cave` (subdirectory)
- `/d/forest/rooms/clearing` (nested subdirectory)

But NOT:
- `/d/forestville` (different path, not a subdirectory)
- `/d/other/forest` (different parent)

### Grant Properties

| Property | Description |
|----------|-------------|
| `id` | Unique grant identifier |
| `universe_id` | Universe the grant applies to |
| `grantee_id` | Account receiving the grant |
| `path_prefix` | Path prefix covered (e.g., `/d/forest`) |
| `can_delegate` | Whether grantee can sub-grant to others |
| `granted_by` | Account that created the grant |
| `granted_at` | Timestamp of grant creation |

### Delegation Rules

- Wizards can grant any path with any delegation rights
- Builders can only grant subpaths of their own grants
- `can_delegate=false` prevents further sub-delegation

## Object Ownership

Every object has an optional `owner_id` field set to the creator's account ID.

**Ownership benefits:**
- Owners can always modify their own objects
- Ownership persists even after grant revocation
- Enables player-created content systems

## Lua API

### Permission Checks

```lua
-- Check if action is allowed on an object
local result = game.check_permission("modify", "/d/forest/cave", false, owner_id)
if result.allowed then
    -- proceed
else
    print("Denied: " .. result.error)
end

-- Check if current user can access a path
if game.can_access_path("/d/forest") then
    -- user has access
end
```

### Managing Access Levels

```lua
-- Get user's access level
local level = game.get_access_level(account_id)  -- "player", "builder", etc.

-- Set user's access level (requires admin)
game.set_access_level(account_id, "wizard")
```

### Managing Path Grants

```lua
-- Grant path access (requires delegation permission)
local grant = game.grant_path(grantee_id, "/d/forest", true)  -- true = can_delegate
if grant.error then
    print("Failed: " .. grant.error)
else
    print("Granted: " .. grant.id)
end

-- Revoke a grant
local revoked = game.revoke_path(grant_id)
if type(revoked) == "table" and revoked.error then
    print("Failed: " .. revoked.error)
end

-- List grants for a user
local grants = game.get_path_grants(account_id)
for i, g in ipairs(grants) do
    print(g.path_prefix .. " (can_delegate=" .. tostring(g.can_delegate) .. ")")
end
```

## Actions

| Action | Description | Required Permission |
|--------|-------------|---------------------|
| `read` | Read object properties | Player |
| `modify` | Change object properties | Owner/PathGrant/Wizard+ |
| `move` | Move object to new location | Owner/PathGrant/Wizard+ (both paths) |
| `delete` | Remove object | Owner/PathGrant/Wizard+ |
| `create` | Create new object | PathGrant/Wizard+ |
| `execute` | Run object code | Player |
| `store_code` | Store code in database | Wizard+ only |
| `admin_config` | Change universe settings | Admin+ |
| `grant_credits` | Grant currency to players | Admin+ |

## Database Schema

### path_grants Table

```sql
CREATE TABLE path_grants (
    id TEXT PRIMARY KEY,
    universe_id TEXT NOT NULL,
    grantee_id TEXT NOT NULL,
    path_prefix TEXT NOT NULL,
    can_delegate BOOLEAN NOT NULL DEFAULT FALSE,
    granted_by TEXT NOT NULL,
    granted_at TEXT NOT NULL,
    UNIQUE(universe_id, grantee_id, path_prefix)
);
```

### objects.owner_id Column

Objects have an `owner_id` column referencing the creator's account.

## Security Notes

1. **Mandatory enforcement**: Permission checks happen in Rust, Lua cannot bypass them
2. **No inheritance abuse**: Path grants explicitly check for `/` boundaries
3. **Audit trail**: Grants record who created them and when
4. **Least privilege**: Start with minimal grants, expand as needed
