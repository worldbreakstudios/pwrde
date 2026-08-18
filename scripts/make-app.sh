#!/usr/bin/env bash
# Assemble Pwrde.app from the release binary.
#
# Usage: scripts/make-app.sh
# Produces: target/release/Pwrde.app  (double-clickable macOS app bundle)
set -euo pipefail

cd "$(dirname "$0")/.."

BIN_NAME="pwrde"
APP_NAME="Pwrde"
BUNDLE_ID="com.pwrde.terminal"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')"

BIN_PATH="target/release/${BIN_NAME}"
APP_DIR="target/release/${APP_NAME}.app"

if [[ ! -f "${BIN_PATH}" ]]; then
  echo "error: ${BIN_PATH} not found — run 'cargo build --release' first" >&2
  exit 1
fi

rm -rf "${APP_DIR}"
mkdir -p "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources"

cp "${BIN_PATH}" "${APP_DIR}/Contents/MacOS/${BIN_NAME}"

# Generate AppIcon.icns from the source PNG (2048x2048 recommended).
ICON_SRC="resources/pwrde-app-icon.png"
if [[ -f "${ICON_SRC}" ]]; then
  ICONSET="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "${ICONSET}"
  for size in 16 32 128 256 512; do
    sips -z "${size}" "${size}" "${ICON_SRC}" --out "${ICONSET}/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "${ICON_SRC}" --out "${ICONSET}/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "${ICONSET}" -o "${APP_DIR}/Contents/Resources/AppIcon.icns"
  rm -rf "$(dirname "${ICONSET}")"
else
  echo "warning: ${ICON_SRC} not found; bundling without an app icon" >&2
fi

cat > "${APP_DIR}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleExecutable</key>
	<string>${BIN_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Folder</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.folder</string>
			</array>
		</dict>
	</array>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>${BUNDLE_ID}</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>pwrde</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
PLIST

echo "APPL????" > "${APP_DIR}/Contents/PkgInfo"

# Ad-hoc codesign so macOS will launch it locally (Gatekeeper still warns on
# first open since it's unsigned by a Developer ID / unnotarized).
codesign --force --deep --sign - "${APP_DIR}" >/dev/null 2>&1 || \
  echo "warning: codesign failed (ad-hoc); app may need a right-click > Open" >&2

echo "built ${APP_DIR} (v${VERSION})"
