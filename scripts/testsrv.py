#!/usr/bin/env python3
"""Local Mudcroft server for manual play-testing.

  scripts/testsrv.py start   create a fresh database in a private temp dir,
                             run mudd serving the built web client, create a
                             universe and load scripts/cave_adventure.lua
                             into it; runs in the foreground until Ctrl+C
                             (or `stop`), then deletes the temp dir
  scripts/testsrv.py stop    stop the server `start` recorded

Environment:
  PORT            port to listen on (default 8080; the Vite dev server's
                  proxy also expects 8080, so `npm run dev` in client/ works
                  against it)
  ADMIN_USERNAME  admin account (default admin)
  ADMIN_PASSWORD  admin password (default testpass123)

Optional KEY=VALUE settings for mudd (such as VENICE_API_KEY) are read from
.env in the repository root.

Needs the built binaries ((cd mudd && cargo build --release)) and client
((cd client && npm ci && npm run build)). Python standard library only.
For automated browser tests use scripts/e2e.py instead.
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BIN_DIR = REPO / "mudd" / "target" / "release"
DIST_DIR = REPO / "client" / "dist"
UNIVERSE_ID = "test-world"
PORTAL = "/rooms/cave-entrance"
TEMP_PREFIX = "mudcroft-testsrv."


def state_file() -> Path:
    """Per-user record of the running server: {"pid", "dir"}"""
    base = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    user = os.environ.get("USER") or str(os.getuid())
    return Path(base) / f"mudcroft-testsrv-{user}.json"


def read_state() -> dict | None:
    try:
        return json.loads(state_file().read_text())
    except (OSError, ValueError):
        return None


def is_mudd(pid: int) -> bool:
    """Whether `pid` is alive and is a mudd process (never signal others)"""
    try:
        return Path(f"/proc/{pid}/comm").read_text().strip() == "mudd"
    except OSError:
        pass
    try:
        out = subprocess.run(
            ["ps", "-o", "comm=", "-p", str(pid)], capture_output=True, text=True
        )
    except OSError:
        return False
    return out.stdout.strip() == "mudd"


def remove_temp_dir(path: str | None) -> None:
    """Delete a directory, but only one that `start` created"""
    if not path:
        return
    p = Path(path)
    if p.parent == Path(tempfile.gettempdir()) and p.name.startswith(TEMP_PREFIX):
        shutil.rmtree(p, ignore_errors=True)


def stop_process(proc_pid: int, wait=None) -> None:
    """SIGTERM (mudd shuts down gracefully), then SIGKILL after 5 s"""
    try:
        os.kill(proc_pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if wait is not None:
            if wait() is not None:
                return
        elif not is_mudd(proc_pid):
            return
        time.sleep(0.1)
    try:
        os.kill(proc_pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def api(base: str, method: str, path: str, body=None, token=None) -> dict:
    """JSON request; raises with the server's message on an HTTP error"""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        raise SystemExit(f"[ERROR] {method} {path}: {e.code} {e.read().decode(errors='replace')}")


def healthy(base: str) -> bool:
    try:
        with urllib.request.urlopen(base + "/health", timeout=1):
            return True
    except (urllib.error.URLError, OSError):
        return False


def dotenv() -> dict:
    """KEY=VALUE lines from the repo's .env (comments and blanks skipped)"""
    env = {}
    path = REPO / ".env"
    if not path.is_file():
        return env
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip().removeprefix("export ").strip()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        env[key] = value
    return env


def interrupt(*_):
    raise KeyboardInterrupt


def start() -> int:
    port = int(os.environ.get("PORT", "8080"))
    admin = os.environ.get("ADMIN_USERNAME", "admin")
    password = os.environ.get("ADMIN_PASSWORD", "testpass123")
    base = f"http://127.0.0.1:{port}"

    for name in ("mudd", "mudd_init"):
        if not os.access(BIN_DIR / name, os.X_OK):
            sys.exit(f"[ERROR] {BIN_DIR / name} not found. Run: (cd mudd && cargo build --release)")
    if not (DIST_DIR / "index.html").is_file():
        sys.exit("[ERROR] client not built. Run: (cd client && npm ci && npm run build)")
    state = read_state()
    if state and is_mudd(state.get("pid", 0)):
        sys.exit(f"[ERROR] A test server is already running (PID {state['pid']}). "
                 "Stop it with: scripts/testsrv.py stop")
    if healthy(base):
        sys.exit(f"[ERROR] Something is already listening on port {port}. Set PORT to use another port.")

    # Ctrl+C and SIGTERM (e.g. `kill`) both clean up. Installed explicitly:
    # a shell may start background jobs with SIGINT ignored.
    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)

    tmp = tempfile.mkdtemp(prefix=TEMP_PREFIX)
    db = Path(tmp) / "mudcroft.db"
    server = None
    log = None
    try:
        print(f"[SETUP] Initializing database in {tmp}...")
        with open(Path(tmp) / "init.log", "w") as log:
            init = subprocess.run(
                [str(BIN_DIR / "mudd_init"), "--database", str(db), "--lib-dir", str(REPO / "mudd" / "lib")],
                env={**os.environ, "MUDD_ADMIN_USERNAME": admin, "MUDD_ADMIN_PASSWORD": password},
                stdout=log, stderr=subprocess.STDOUT,
            )
        if init.returncode != 0:
            print((Path(tmp) / "init.log").read_text())
            return 1

        print(f"[START] Starting mudd on {base}...")
        log = open(Path(tmp) / "server.log", "w")
        # run_script reads scripts/ relative to the working directory
        server = subprocess.Popen(
            [str(BIN_DIR / "mudd"), "--bind", f"127.0.0.1:{port}", "--database", str(db),
             "--static-dir", str(DIST_DIR)],
            cwd=REPO, env={**os.environ, **dotenv()}, stdout=log, stderr=subprocess.STDOUT,
        )
        state_file().write_text(json.dumps({"pid": server.pid, "dir": tmp}))

        deadline = time.monotonic() + 30
        while not healthy(base):
            if server.poll() is not None or time.monotonic() > deadline:
                print((Path(tmp) / "server.log").read_text())
                print("[ERROR] server did not start")
                return 2
            time.sleep(0.1)

        print(f"[SEED] Logging in as {admin}...")
        login = api(base, "POST", "/auth/login", {"username": admin, "password": password})
        token = login["token"]
        print(f"[SEED] Creating universe {UNIVERSE_ID}...")
        api(base, "POST", "/universe/create",
            {"id": UNIVERSE_ID, "name": "Test Universe", "owner_id": login["account_id"]}, token)
        print("[SEED] Loading cave adventure...")
        print(api(base, "POST", f"/universe/{UNIVERSE_ID}/run_script",
                  {"script": "cave_adventure.lua"}, token).get("result"))
        print("[SEED] Setting portal...")
        api(base, "POST", f"/universe/{UNIVERSE_ID}/portal", {"room_id": PORTAL}, token)

        print(f"""
=========================================
   READY FOR PLAY-TESTING
=========================================
  Open:      {base}
  Admin:     {admin} / {password}
  Universe:  {UNIVERSE_ID}
  Logs:      {tmp}/server.log
  Stop:      Ctrl+C, or scripts/testsrv.py stop
=========================================""", flush=True)

        # 0 when stopped (mudd exits 0 on SIGTERM); otherwise it crashed
        return server.wait()
    except KeyboardInterrupt:
        return 0
    finally:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        print("\n[CLEANUP] Stopping server...")
        if server is not None and server.poll() is None:
            stop_process(server.pid, wait=server.poll)
            server.wait()
        if log is not None:
            log.close()
        remove_temp_dir(tmp)
        state = read_state()
        if state and state.get("dir") == tmp:
            state_file().unlink(missing_ok=True)
        print("[CLEANUP] Done")


def stop() -> int:
    state = read_state()
    if not state:
        print("[STOP] No test server recorded")
        return 0
    pid = state.get("pid", 0)
    if is_mudd(pid):
        print(f"[STOP] Stopping mudd (PID {pid})...")
        stop_process(pid)
    else:
        print(f"[STOP] Recorded server (PID {pid}) is not running")
    # `start` cleans up when mudd exits; this covers a `start` that is gone
    remove_temp_dir(state.get("dir"))
    state_file().unlink(missing_ok=True)
    print("[STOP] Done")
    return 0


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in ("start", "stop"):
        print(__doc__.strip(), file=sys.stderr)
        return 2
    return start() if sys.argv[1] == "start" else stop()


if __name__ == "__main__":
    sys.exit(main())
