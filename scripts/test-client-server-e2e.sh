#!/bin/bash
#
# E2E Test Server Setup Script
#
# Sets up the server infrastructure for browser-based E2E testing.
# After running this script, use Claude Chrome browser automation
# to perform the actual tests.
#
# Usage:
#   ./scripts/test-client-server-e2e.sh
#
# What it does:
#   1. Cleans test directory
#   2. Initializes database with admin user
#   3. Starts server
#   4. Creates default universe
#   5. Starts client dev server
#   6. Waits for browser-based testing (Ctrl+C to stop)
#
# Environment:
#   TESTDIR - Test directory (default: /tmp/mudcroft-e2e-test)
#

set -e

# Configuration
TESTDIR="${TESTDIR:-/tmp/mudcroft-e2e-test}"
ADMIN_USERNAME="admin"
ADMIN_PASSWORD="testpass123"
SERVER_HOST="127.0.0.1"
SERVER_PORT="8080"
SERVER_URL="http://${SERVER_HOST}:${SERVER_PORT}"

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# PIDs for cleanup
SERVER_PID=""
CLIENT_PID=""

cleanup() {
    echo ""
    echo "[CLEANUP] Stopping services..."

    if [ -n "$CLIENT_PID" ] && kill -0 "$CLIENT_PID" 2>/dev/null; then
        kill "$CLIENT_PID" 2>/dev/null || true
        wait "$CLIENT_PID" 2>/dev/null || true
        echo "[CLEANUP] Client stopped"
    fi

    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        echo "[CLEANUP] Server stopped"
    fi

    if [ -d "$TESTDIR" ]; then
        rm -rf "$TESTDIR"
        echo "[CLEANUP] Test directory removed"
    fi

    echo "[CLEANUP] Done"
}

trap cleanup EXIT

wait_for_server() {
    local max_attempts=50
    local attempt=0

    while [ $attempt -lt $max_attempts ]; do
        if curl -s "${SERVER_URL}/health" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
        ((attempt++))
    done

    return 1
}

main() {
    echo ""
    echo "========================================="
    echo "   HemiMUD E2E Test Server Setup"
    echo "========================================="
    echo ""

    # Check prerequisites
    if [ ! -f "${PROJECT_ROOT}/mudd/target/release/mudd" ]; then
        echo "[ERROR] mudd binary not found. Run: cd mudd && cargo build --release"
        exit 1
    fi

    if [ ! -f "${PROJECT_ROOT}/mudd/target/release/mudd_init" ]; then
        echo "[ERROR] mudd_init binary not found. Run: cd mudd && cargo build --release"
        exit 1
    fi

    # Phase 1: Setup
    echo "[SETUP] Cleaning test directory..."
    rm -rf "$TESTDIR"
    mkdir -p "$TESTDIR"

    # Phase 2: Initialize Database
    echo "[SETUP] Initializing database..."
    MUDD_ADMIN_USERNAME="$ADMIN_USERNAME" \
    MUDD_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    "${PROJECT_ROOT}/mudd/target/release/mudd_init" \
        --database "${TESTDIR}/mudcroft.db" 2>&1 | grep -v "^$"

    # Phase 3: Start Server
    echo "[START] Starting server..."

    # Load .env if exists
    if [ -f "${PROJECT_ROOT}/.env" ]; then
        set -a
        source "${PROJECT_ROOT}/.env"
        set +a
    fi

    "${PROJECT_ROOT}/mudd/target/release/mudd" \
        --bind "${SERVER_HOST}:${SERVER_PORT}" \
        --database "${TESTDIR}/mudcroft.db" \
        >"${TESTDIR}/server.log" 2>&1 &
    SERVER_PID=$!

    if ! wait_for_server; then
        echo "[ERROR] Server failed to start"
        cat "${TESTDIR}/server.log"
        exit 2
    fi
    echo "[START] Server running (PID: $SERVER_PID)"

    # Phase 4: Create Universe via API
    echo "[START] Logging in as admin..."
    LOGIN_RESPONSE=$(curl -s -X POST "${SERVER_URL}/auth/login" \
        -H "Content-Type: application/json" \
        -d "{\"username\":\"${ADMIN_USERNAME}\",\"password\":\"${ADMIN_PASSWORD}\"}")

    ADMIN_UUID=$(echo "$LOGIN_RESPONSE" | jq -r '.account_id')
    if [ -z "$ADMIN_UUID" ] || [ "$ADMIN_UUID" = "null" ]; then
        echo "[ERROR] Login failed: $LOGIN_RESPONSE"
        exit 2
    fi

    echo "[START] Creating default universe..."
    UNIVERSE_RESPONSE=$(curl -s -X POST "${SERVER_URL}/universe/create" \
        -H "Content-Type: application/json" \
        -d "{\"id\":\"default\",\"name\":\"Test Universe\",\"owner_id\":\"${ADMIN_UUID}\"}")

    UNIVERSE_ID=$(echo "$UNIVERSE_RESPONSE" | jq -r '.id')
    if [ -z "$UNIVERSE_ID" ] || [ "$UNIVERSE_ID" = "null" ]; then
        echo "[ERROR] Universe creation failed: $UNIVERSE_RESPONSE"
        exit 2
    fi
    echo "[START] Universe created: $UNIVERSE_ID"

    # Phase 5: Start Client
    echo "[START] Starting client..."
    (cd "${PROJECT_ROOT}/client" && npm run dev >"${TESTDIR}/client.log" 2>&1) &
    CLIENT_PID=$!
    sleep 2

    if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
        echo "[WARN] Client failed to start"
        CLIENT_PID=""
    else
        echo "[START] Client running (PID: $CLIENT_PID)"
    fi

    # Ready
    echo ""
    echo "========================================="
    echo "   READY FOR BROWSER TESTING"
    echo "========================================="
    echo ""
    echo "  Server:  ${SERVER_URL}"
    echo "  Client:  http://localhost:5173"
    echo "  Admin:   ${ADMIN_USERNAME} / ${ADMIN_PASSWORD}"
    echo ""
    echo "  Press Ctrl+C to stop and cleanup"
    echo ""
    echo "========================================="

    # Wait for interrupt
    wait
}

main "$@"
