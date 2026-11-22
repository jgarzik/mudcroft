#!/usr/bin/env python3
"""End-to-end browser tests: build the server and web client, then run the
Playwright suite (headless Chromium against a throwaway mudd).

Usage: scripts/e2e.py [playwright args...]
  e.g. scripts/e2e.py e2e/gameplay.spec.ts --headed

The suite starts its own mudd on a random port in a temp dir and removes it
afterwards (set MUDCROFT_E2E_KEEP=1 to keep the temp dir and logs).

Requirements: cargo, node/npm. Playwright's Chromium is installed with
  (cd client && npx playwright install chromium)
and, on a fresh machine, its system libraries with
  (cd client && sudo npx playwright install-deps chromium)
Python standard library only.
"""

import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def run(cmd: list[str], cwd: Path) -> None:
    result = subprocess.run(cmd, cwd=cwd)
    if result.returncode != 0:
        sys.exit(result.returncode)


def main() -> None:
    print("==> Building mudd (release)", flush=True)
    run(["cargo", "build", "--release", "--bins"], REPO / "mudd")

    client = REPO / "client"
    print("==> Building web client", flush=True)
    if not (client / "node_modules").is_dir():
        run(["npm", "ci"], client)
    run(["npm", "run", "build"], client)

    print("==> Running Playwright", flush=True)
    os.chdir(client)
    os.execvp("npx", ["npx", "playwright", "test", *sys.argv[1:]])


if __name__ == "__main__":
    main()
