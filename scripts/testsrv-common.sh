# shellcheck shell=bash
# Shared by start-testsrv.sh and stop-testsrv.sh (sourced, not executed).
#
# The running test server is recorded in a per-user state file:
#   line 1: mudd PID
#   line 2: temp dir
# Only that PID is ever signalled; nothing is killed by port or name.

TESTSRV_STATE="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/mudcroft-testsrv-${USER:-$(id -u)}.state"

# PID recorded in the state file (empty if none)
testsrv_pid() {
    [ -f "$TESTSRV_STATE" ] && sed -n 1p "$TESTSRV_STATE"
}

# Temp dir recorded in the state file (empty if none)
testsrv_dir() {
    [ -f "$TESTSRV_STATE" ] && sed -n 2p "$TESTSRV_STATE"
}

# True if the recorded PID is alive and is a mudd process
testsrv_running() {
    local pid
    pid="$(testsrv_pid)"
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null &&
        [ "$(ps -o comm= -p "$pid" 2>/dev/null)" = "mudd" ]
}
