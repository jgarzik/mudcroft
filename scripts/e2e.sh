#!/usr/bin/env bash
#
# End-to-end browser tests: build the server and web client, then run the
# Playwright suite (headless Chromium against a throwaway mudd).
#
# Usage: scripts/e2e.sh [playwright args...]
#   e.g. scripts/e2e.sh e2e/gameplay.spec.ts --headed
#
# The suite starts its own mudd on a random port in a temp dir and removes
# it afterwards (set MUDCROFT_E2E_KEEP=1 to keep the temp dir and logs).
#
# Requirements: cargo, node/npm. Playwright's Chromium is installed with
#   (cd client && npx playwright install chromium)
# and, on a fresh machine, its system libraries with
#   (cd client && sudo npx playwright install-deps chromium)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> Building mudd (release)"
(cd "$REPO_DIR/mudd" && cargo build --release --bins)

echo "==> Building web client"
cd "$REPO_DIR/client"
[ -d node_modules ] || npm ci
npm run build

echo "==> Running Playwright"
exec npx playwright test "$@"
