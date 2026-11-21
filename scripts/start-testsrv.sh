#!/usr/bin/env bash
#
# Start a local Mudcroft server for manual play-testing.
#
# Creates a fresh database in a private temp dir, starts mudd serving the
# built web client (client/dist) on the same origin, creates a universe and
# loads scripts/cave_adventure.lua into it. Runs in the foreground; Ctrl+C
# (or scripts/stop-testsrv.sh from another terminal) stops the server and
# deletes the temp dir.
#
# Usage:
#   scripts/start-testsrv.sh
#
# Environment:
#   PORT            port to listen on (default 8080; the Vite dev server's
#                   proxy also expects 8080, so `npm run dev` in client/ works
#                   against it)
#   ADMIN_USERNAME  admin account (default admin)
#   ADMIN_PASSWORD  admin password (default testpass123)
#
# Prerequisites: built binaries (cd mudd && cargo build --release), built
# client (cd client && npm run build), curl, jq, node (for setportal).
# For automated browser tests use scripts/e2e.sh instead.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
# shellcheck source-path=SCRIPTDIR source=testsrv-common.sh
source "${SCRIPT_DIR}/testsrv-common.sh"

PORT="${PORT:-8080}"
ADMIN_USERNAME="${ADMIN_USERNAME:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-testpass123}"
UNIVERSE_ID="test-world"
SERVER_URL="http://127.0.0.1:${PORT}"
BIN_DIR="${PROJECT_ROOT}/mudd/target/release"
DIST_DIR="${PROJECT_ROOT}/client/dist"

for tool in curl jq node; do
    command -v "$tool" >/dev/null || { echo "[ERROR] $tool is required"; exit 1; }
done
for bin in mudd mudd_init; do
    [ -x "${BIN_DIR}/${bin}" ] || {
        echo "[ERROR] ${BIN_DIR}/${bin} not found. Run: (cd mudd && cargo build --release)"
        exit 1
    }
done
[ -f "${DIST_DIR}/index.html" ] || {
    echo "[ERROR] client not built. Run: (cd client && npm ci && npm run build)"
    exit 1
}
if testsrv_running; then
    echo "[ERROR] A test server is already running (PID $(testsrv_pid)). Stop it with scripts/stop-testsrv.sh"
    exit 1
fi
if curl -s -o /dev/null "${SERVER_URL}/health"; then
    echo "[ERROR] Something is already listening on port ${PORT}. Set PORT to use another port."
    exit 1
fi

TESTDIR="$(mktemp -d "${TMPDIR:-/tmp}/mudcroft-testsrv.XXXXXX")"
SERVER_PID=""

# shellcheck disable=SC2317  # invoked via trap
cleanup() {
    trap - EXIT INT TERM
    echo ""
    echo "[CLEANUP] Stopping server..."
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$TESTDIR"
    rm -f "$TESTSRV_STATE"
    echo "[CLEANUP] Done"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "[SETUP] Initializing database in ${TESTDIR}..."
MUDD_ADMIN_USERNAME="$ADMIN_USERNAME" MUDD_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    "${BIN_DIR}/mudd_init" --database "${TESTDIR}/mudcroft.db" \
    --lib-dir "${PROJECT_ROOT}/mudd/lib" >"${TESTDIR}/init.log" 2>&1 || {
    cat "${TESTDIR}/init.log"
    exit 1
}

# Optional local settings such as VENICE_API_KEY
if [ -f "${PROJECT_ROOT}/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    source "${PROJECT_ROOT}/.env"
    set +a
fi

echo "[START] Starting mudd on ${SERVER_URL}..."
# run_script reads scripts/ relative to the working directory
(cd "$PROJECT_ROOT" && exec "${BIN_DIR}/mudd" \
    --bind "127.0.0.1:${PORT}" \
    --database "${TESTDIR}/mudcroft.db" \
    --static-dir "$DIST_DIR") >"${TESTDIR}/server.log" 2>&1 &
SERVER_PID=$!
printf '%s\n%s\n' "$SERVER_PID" "$TESTDIR" >"$TESTSRV_STATE"

for _ in $(seq 1 300); do
    curl -s -o /dev/null "${SERVER_URL}/health" && break
    kill -0 "$SERVER_PID" 2>/dev/null || { cat "${TESTDIR}/server.log"; exit 2; }
    sleep 0.1
done
curl -s -o /dev/null "${SERVER_URL}/health" || { echo "[ERROR] server did not start"; exit 2; }

api() { # method path [json] -> body; fails on HTTP errors
    curl -sf -X "$1" "${SERVER_URL}$2" -H "Content-Type: application/json" \
        ${TOKEN:+-H "Authorization: Bearer ${TOKEN}"} ${3:+-d "$3"}
}

echo "[SEED] Logging in as ${ADMIN_USERNAME}..."
LOGIN="$(api POST /auth/login "{\"username\":\"${ADMIN_USERNAME}\",\"password\":\"${ADMIN_PASSWORD}\"}")"
TOKEN="$(jq -r .token <<<"$LOGIN")"
ADMIN_ID="$(jq -r .account_id <<<"$LOGIN")"

echo "[SEED] Creating universe ${UNIVERSE_ID}..."
api POST /universe/create \
    "{\"id\":\"${UNIVERSE_ID}\",\"name\":\"Test Universe\",\"owner_id\":\"${ADMIN_ID}\"}" >/dev/null

echo "[SEED] Loading cave adventure..."
api POST "/universe/${UNIVERSE_ID}/run_script" '{"script":"cave_adventure.lua"}' | jq -r .result

echo "[SEED] Setting portal..."
WS_URL="ws://127.0.0.1:${PORT}/ws?universe=${UNIVERSE_ID}&token=${TOKEN}" node -e '
const ws = new WebSocket(process.env.WS_URL)
// The reply is the first output/error after our command is echoed back
let sent = false
let echoed = false
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data)
  if (msg.type === "welcome" && !sent) {
    sent = true
    ws.send(JSON.stringify({ type: "command", text: "setportal /rooms/cave-entrance" }))
  } else if (msg.type === "echo") {
    echoed = true
  } else if (echoed && (msg.type === "output" || msg.type === "error")) {
    console.log(msg.text || msg.message)
    ws.close()
    process.exit(msg.type === "error" ? 1 : 0)
  }
}
setTimeout(() => { console.error("setportal timed out"); process.exit(1) }, 10000)
'

cat <<EOF

=========================================
   READY FOR PLAY-TESTING
=========================================
  Open:      ${SERVER_URL}
  Admin:     ${ADMIN_USERNAME} / ${ADMIN_PASSWORD}
  Universe:  ${UNIVERSE_ID}
  Logs:      ${TESTDIR}/server.log
  Stop:      Ctrl+C, or scripts/stop-testsrv.sh
=========================================
EOF

# Exit 0 when stopped by stop-testsrv.sh (SIGTERM); report crashes
status=0
wait "$SERVER_PID" || status=$?
[ "$status" -eq 143 ] && status=0
exit "$status"
