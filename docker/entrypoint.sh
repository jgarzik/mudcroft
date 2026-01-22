#!/bin/bash
set -e

# HemiMUD Docker Entrypoint Script
# Initializes database and starts supervisord

echo "========================================="
echo "HemiMUD Container Initialization"
echo "========================================="

# Environment variable defaults
DB_PATH="${MUDD_DATABASE_PATH:-/data/mudcroft.db}"
LIB_DIR="${MUDD_LIB_DIR:-/app/lib}"
BIND_ADDR="${MUDD_BIND_ADDR:-127.0.0.1:8080}"

echo "Database path: $DB_PATH"
echo "Library dir: $LIB_DIR"
echo "Bind address: $BIND_ADDR"

# Check if this is a new database
if [ ! -f "$DB_PATH" ]; then
    echo ""
    echo "New database detected. Initializing..."
    echo ""

    # Validate admin credentials are provided
    if [ -z "$MUDD_ADMIN_USERNAME" ]; then
        echo "ERROR: MUDD_ADMIN_USERNAME environment variable is required for new database"
        echo "Usage: docker run -e MUDD_ADMIN_USERNAME=admin -e MUDD_ADMIN_PASSWORD=secret ..."
        exit 1
    fi

    if [ -z "$MUDD_ADMIN_PASSWORD" ]; then
        echo "ERROR: MUDD_ADMIN_PASSWORD environment variable is required for new database"
        echo "Usage: docker run -e MUDD_ADMIN_USERNAME=admin -e MUDD_ADMIN_PASSWORD=secret ..."
        exit 1
    fi

    echo "Creating new database with admin user: $MUDD_ADMIN_USERNAME"
else
    echo ""
    echo "Existing database found. Running upgrade check..."
    echo ""
fi

# Run mudd_init (idempotent - safe for both new and existing databases)
echo "Running mudd_init..."
/app/mudd_init \
    --database "$DB_PATH" \
    --lib-dir "$LIB_DIR"

if [ $? -ne 0 ]; then
    echo ""
    echo "ERROR: Database initialization failed"
    exit 1
fi

echo ""
echo "Database initialization complete"
echo ""

# Verify database file exists and is readable
if [ ! -r "$DB_PATH" ]; then
    echo "ERROR: Database file $DB_PATH is not readable"
    exit 1
fi

echo "========================================="
echo "Starting HemiMUD Services"
echo "========================================="
echo ""
echo "Services:"
echo "  - nginx (port 80)"
echo "  - mudd ($BIND_ADDR)"
echo ""

# Start supervisord (replaces this process)
exec /usr/bin/supervisord -c /etc/supervisor/conf.d/supervisord.conf
