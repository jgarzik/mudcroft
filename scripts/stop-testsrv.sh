#!/bin/bash
#
# Stop Test Server Script
#
# Stops any running test server and client processes.
#
# Usage:
#   ./scripts/stop-testsrv.sh
#

# Configuration
TESTDIR="${TESTDIR:-/tmp/mudcroft-e2e-test}"
SERVER_PORT="8080"

# Script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Kill processes listening on server port
if command -v lsof >/dev/null 2>&1; then
    PIDS=$(lsof -ti :${SERVER_PORT} 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        echo "[STOP] Killing processes on port ${SERVER_PORT}..."
        echo "$PIDS" | xargs kill 2>/dev/null || true
        sleep 0.5
    fi
fi

# Kill any mudd processes running from test directory
MUDD_PIDS=$(pgrep -f "mudd.*${TESTDIR}" 2>/dev/null || true)
if [ -n "$MUDD_PIDS" ]; then
    echo "[STOP] Killing test server processes..."
    echo "$MUDD_PIDS" | xargs kill 2>/dev/null || true
fi

# Kill client dev server (npm run dev from client directory)
CLIENT_PIDS=$(pgrep -f "vite.*${PROJECT_ROOT}/client" 2>/dev/null || true)
if [ -n "$CLIENT_PIDS" ]; then
    echo "[STOP] Killing client dev server..."
    echo "$CLIENT_PIDS" | xargs kill 2>/dev/null || true
fi

# Also try killing by the npm process
NPM_PIDS=$(pgrep -f "npm.*dev.*client" 2>/dev/null || true)
if [ -n "$NPM_PIDS" ]; then
    echo "[STOP] Killing npm processes..."
    echo "$NPM_PIDS" | xargs kill 2>/dev/null || true
fi

# Clean up test directory
if [ -d "$TESTDIR" ]; then
    rm -rf "$TESTDIR"
    echo "[STOP] Test directory removed"
fi

echo "[STOP] Done"
