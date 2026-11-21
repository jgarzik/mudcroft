#!/usr/bin/env bash
#
# Stop the test server started by scripts/start-testsrv.sh.
#
# Signals only the mudd PID recorded by start-testsrv.sh (never "whatever is
# on port 8080") and removes its temp dir. The start script then exits and
# cleans up after itself as well.
#
# Usage:
#   scripts/stop-testsrv.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=testsrv-common.sh
source "${SCRIPT_DIR}/testsrv-common.sh"

if [ ! -f "$TESTSRV_STATE" ]; then
    echo "[STOP] No test server recorded"
    exit 0
fi

PID="$(testsrv_pid)"
DIR="$(testsrv_dir)"

if testsrv_running; then
    echo "[STOP] Stopping mudd (PID ${PID})..."
    kill "$PID"
    for _ in $(seq 1 50); do
        kill -0 "$PID" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$PID" 2>/dev/null; then
        kill -9 "$PID" 2>/dev/null || true
    fi
else
    echo "[STOP] Recorded server (PID ${PID:-?}) is not running"
fi

# Only remove what start-testsrv.sh created
case "$DIR" in
    "${TMPDIR:-/tmp}"/mudcroft-testsrv.*) rm -rf "$DIR" ;;
esac
rm -f "$TESTSRV_STATE"
echo "[STOP] Done"
