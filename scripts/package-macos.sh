#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Seoul"
BUNDLE_ID="sh.superset.seoul"
MIN_MACOS_VERSION="13.0"
TARGET_TRIPLE="${SEOUL_TARGET_TRIPLE:-aarch64-apple-darwin}"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "error: macOS packaging must run on Darwin" >&2
    exit 1
fi

if [[ "$TARGET_TRIPLE" != "aarch64-apple-darwin" ]]; then
    echo "error: this packager currently supports only aarch64-apple-darwin" >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    case "$CARGO_TARGET_DIR" in
        /*) TARGET_DIR="$CARGO_TARGET_DIR" ;;
        *) TARGET_DIR="$REPO_ROOT/$CARGO_TARGET_DIR" ;;
    esac
else
    TARGET_DIR="$REPO_ROOT/target"
fi

APP_VERSION="$(cargo pkgid -p seoul-terminal)"
APP_VERSION="${APP_VERSION##*#}"
BUILD_NUMBER="${SEOUL_BUILD_NUMBER:-$(git rev-list --count HEAD)}"
GIT_SHA="$(git rev-parse --short=7 HEAD)"
GIT_TAG="seoul-v${APP_VERSION}"
DIRTY_SUFFIX=""
if [[ -n "$(git status --porcelain)" ]]; then
    DIRTY_SUFFIX="-dirty"
fi

ARCH_LABEL="macos-arm64"
if [[ "$(git tag --points-at HEAD | grep -Fx "$GIT_TAG" || true)" == "$GIT_TAG" && -z "$DIRTY_SUFFIX" ]]; then
    ZIP_BASENAME="${APP_NAME}-${APP_VERSION}-${ARCH_LABEL}.zip"
else
    ZIP_BASENAME="${APP_NAME}-${APP_VERSION}-${GIT_SHA}${DIRTY_SUFFIX}-${ARCH_LABEL}.zip"
fi

echo "Building ${APP_NAME} ${APP_VERSION} (${BUILD_NUMBER}) for ${TARGET_TRIPLE}"
cargo build --release --target "$TARGET_TRIPLE" --package seoul-terminal --package seoul-daemon

BIN_DIR="$TARGET_DIR/$TARGET_TRIPLE/release"
BUILD_DIR="$TARGET_DIR/$TARGET_TRIPLE/release/build"
DIST_DIR="$TARGET_DIR/dist"
APP_BUNDLE="$DIST_DIR/${APP_NAME}.app"
CONTENTS_DIR="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
FRAMEWORKS_DIR="$CONTENTS_DIR/Frameworks"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
APP_EXECUTABLE="$MACOS_DIR/seoul"
DAEMON_EXECUTABLE="$MACOS_DIR/seoul-daemon"
GHOSTTY_DYLIB="$FRAMEWORKS_DIR/libghostty-vt.dylib"
ZIP_PATH="$DIST_DIR/$ZIP_BASENAME"

SOURCE_GHOSTTY_DYLIB="$(
    find "$BUILD_DIR" -path "*/ghostty-install/lib/libghostty-vt.dylib" \( -type f -o -type l \) -print 2>/dev/null | sort | tail -n 1
)"
if [[ -z "$SOURCE_GHOSTTY_DYLIB" ]]; then
    SOURCE_GHOSTTY_DYLIB="$(
        find "$BUILD_DIR" -path "*/ghostty-install/lib/libghostty-vt.*.dylib" -type f -print 2>/dev/null | sort | tail -n 1
    )"
fi

if [[ ! -x "$BIN_DIR/seoul" ]]; then
    echo "error: missing release binary: $BIN_DIR/seoul" >&2
    exit 1
fi
if [[ ! -x "$BIN_DIR/seoul-daemon" ]]; then
    echo "error: missing release binary: $BIN_DIR/seoul-daemon" >&2
    exit 1
fi
if [[ -z "$SOURCE_GHOSTTY_DYLIB" || ! -f "$SOURCE_GHOSTTY_DYLIB" ]]; then
    echo "error: could not find libghostty-vt.dylib under $BUILD_DIR" >&2
    exit 1
fi

rm -rf "$APP_BUNDLE" "$ZIP_PATH"
mkdir -p "$MACOS_DIR" "$FRAMEWORKS_DIR" "$RESOURCES_DIR"

cp "$BIN_DIR/seoul" "$APP_EXECUTABLE"
cp "$BIN_DIR/seoul-daemon" "$DAEMON_EXECUTABLE"
cp -L "$SOURCE_GHOSTTY_DYLIB" "$GHOSTTY_DYLIB"
chmod 755 "$APP_EXECUTABLE" "$DAEMON_EXECUTABLE" "$GHOSTTY_DYLIB"

cat > "$CONTENTS_DIR/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleExecutable</key>
    <string>seoul</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${APP_VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${BUILD_NUMBER}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key>
    <string>${MIN_MACOS_VERSION}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF
printf "APPL????" > "$CONTENTS_DIR/PkgInfo"

plutil -lint "$CONTENTS_DIR/Info.plist"

if ! otool -l "$APP_EXECUTABLE" | grep -q "@executable_path/../Frameworks"; then
    install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP_EXECUTABLE"
fi

if otool -L "$APP_EXECUTABLE" "$DAEMON_EXECUTABLE" | grep -q "/opt/homebrew"; then
    echo "error: packaged binaries still reference Homebrew dylibs" >&2
    otool -L "$APP_EXECUTABLE" "$DAEMON_EXECUTABLE" >&2
    exit 1
fi

if ! otool -L "$APP_EXECUTABLE" | grep -q "@rpath/libghostty-vt.dylib"; then
    echo "error: seoul binary does not link against @rpath/libghostty-vt.dylib" >&2
    otool -L "$APP_EXECUTABLE" >&2
    exit 1
fi

codesign --force --sign - "$GHOSTTY_DYLIB"
codesign --force --sign - "$DAEMON_EXECUTABLE"
codesign --force --sign - "$APP_EXECUTABLE"
codesign --force --sign - "$APP_BUNDLE"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

(
    cd "$DIST_DIR"
    ditto -c -k --keepParent --norsrc --noextattr "${APP_NAME}.app" "$ZIP_BASENAME"
)

echo "Packaged $APP_BUNDLE"
echo "Created $ZIP_PATH"
