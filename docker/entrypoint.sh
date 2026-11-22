#!/bin/bash
# Mudcroft container entrypoint: create or upgrade the database with
# mudd_init, then run mudd in its place. Both run as appuser; root is used
# only to make the data directory appuser's (a bind mount may be root-owned).
set -euo pipefail

DB_PATH="${MUDD_DATABASE_PATH:-/data/mudcroft.db}"
LIB_DIR="${MUDD_LIB_DIR:-/app/lib}"
BIND_ADDR="${MUDD_BIND_ADDR:-0.0.0.0:8080}"

# Prefix that runs a command as appuser (empty if already unprivileged);
# setpriv execs the command itself, so mudd ends up as PID 1
AS_APP=()
if [ "$(id -u)" = 0 ]; then
    AS_APP=(setpriv --reuid=appuser --regid=appuser --init-groups --)
fi

if [ "$(id -u)" = 0 ]; then
    # Never follow symlinks here: /data is writable by appuser, so a link
    # planted there must not make root chown some other file
    chown -h appuser:appuser "$(dirname "$DB_PATH")"
    # Database files left root-owned (an older image, mudd_admin run as root)
    for f in "$DB_PATH" "$DB_PATH-wal" "$DB_PATH-shm"; do
        if [ -f "$f" ] && [ ! -L "$f" ]; then chown -h appuser:appuser "$f"; fi
    done
fi

if [ ! -f "$DB_PATH" ]; then
    if [ -z "${MUDD_ADMIN_USERNAME:-}" ] || [ -z "${MUDD_ADMIN_PASSWORD:-}" ]; then
        echo "A new database needs MUDD_ADMIN_USERNAME and MUDD_ADMIN_PASSWORD:" >&2
        echo "  docker run -e MUDD_ADMIN_USERNAME=admin -e MUDD_ADMIN_PASSWORD=... ..." >&2
        exit 1
    fi
    echo "Creating database $DB_PATH (admin: $MUDD_ADMIN_USERNAME)"
else
    echo "Checking database $DB_PATH"
fi

# Creates a new database, or upgrades an existing one (idempotent)
"${AS_APP[@]}" /app/mudd_init --database "$DB_PATH" --lib-dir "$LIB_DIR"

echo "Starting mudd on $BIND_ADDR"
exec "${AS_APP[@]}" /app/mudd --bind "$BIND_ADDR" --database "$DB_PATH" --static-dir /app/static
