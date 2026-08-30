#!/usr/bin/env bash
# Full release: pull latest main, build, bundle, install the app to
# /Applications and the `pwrde-cli` command-bus client onto PATH.
#
# Usage: scripts/deploy.sh
# Refuses to run unless the checkout is on the `main` branch.
# The CLI goes to $PWRDE_CLI_DIR if set, else ~/.cargo/bin if it exists,
# else /usr/local/bin.
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="Pwrde"
APP_DIR="target/release/${APP_NAME}.app"
DEST="/Applications/${APP_NAME}.app"
CLI_NAME="pwrde-cli"
CLI_BIN="target/release/${CLI_NAME}"
if [[ -n "${PWRDE_CLI_DIR:-}" ]]; then
  CLI_DIR="${PWRDE_CLI_DIR}"
elif [[ -d "${HOME}/.cargo/bin" ]]; then
  CLI_DIR="${HOME}/.cargo/bin"
else
  CLI_DIR="/usr/local/bin"
fi

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

echo "installing ${CLI_NAME} to ${CLI_DIR}..."
mkdir -p "${CLI_DIR}"
install -m 0755 "${CLI_BIN}" "${CLI_DIR}/${CLI_NAME}"
if ! command -v "${CLI_NAME}" >/dev/null 2>&1; then
  echo "warning: ${CLI_DIR} is not on PATH; add it so agents can run ${CLI_NAME}" >&2
fi

echo "deployed ${DEST} and ${CLI_DIR}/${CLI_NAME}"
