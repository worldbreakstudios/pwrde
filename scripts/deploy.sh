#!/usr/bin/env bash
# Full release: pull latest main, build, bundle, and install to /Applications.
#
# Usage: scripts/deploy.sh
# Refuses to run unless the checkout is on the `main` branch.
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="Pwrde"
APP_DIR="target/release/${APP_NAME}.app"
DEST="/Applications/${APP_NAME}.app"

# Only deploy from a clean, up-to-date main checkout.
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "${BRANCH}" != "main" ]]; then
  echo "error: deploy must run on 'main' (currently on '${BRANCH}')" >&2
  exit 1
fi

# Stash local changes so the pull applies cleanly; restore them afterward.
STASHED=0
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "stashing local changes..."
  git stash push --include-untracked --message "deploy.sh auto-stash"
  STASHED=1
fi

restore_stash() {
  if [[ "${STASHED}" -eq 1 ]]; then
    echo "restoring stashed changes..."
    git stash pop || echo "warning: 'git stash pop' hit conflicts; resolve manually" >&2
  fi
}
trap restore_stash EXIT

echo "pulling latest main..."
git pull --ff-only origin main

echo "building release binary..."
cargo build --release

echo "bundling app..."
scripts/make-app.sh

echo "installing to ${DEST}..."
rm -rf "${DEST}"
cp -R "${APP_DIR}" "${DEST}"

echo "deployed ${DEST}"
