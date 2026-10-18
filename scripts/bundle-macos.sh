#!/usr/bin/env bash
# Сборка crabtrap.app и .dmg для macOS.
#
#   scripts/bundle-macos.sh [--universal] [--skip-build]
#
# Подпись: если задан MACOS_SIGN_IDENTITY ("Developer ID Application: …") —
# Developer ID + hardened runtime; иначе ad-hoc ("-").
# Нотаризация: если заданы APPLE_API_KEY_PATH, APPLE_API_KEY_ID и
# APPLE_API_ISSUER (App Store Connect API key) и подпись — Developer ID.
#
# Результат: dist/crabtrap.app, dist/crabtrap-<version>-macos-<arch>.dmg(+.sha256).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGING="$ROOT/packaging/macos"
DIST="$ROOT/dist"
BUNDLE_ID="${CRABTRAP_BUNDLE_ID:-io.github.underherzen.crabtrap}"
SIGN_IDENTITY="${MACOS_SIGN_IDENTITY:--}"
ARM_TARGET="aarch64-apple-darwin"
X86_TARGET="x86_64-apple-darwin"

universal=false
skip_build=false
for arg in "$@"; do
    case "$arg" in
        --universal) universal=true ;;
        --skip-build) skip_build=true ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

workspace_version() {
    cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" |
        python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="app"))'
}

build_binary() {
    local targets=("$ARM_TARGET")
    $universal && targets+=("$X86_TARGET")
    if ! $skip_build; then
        for target in "${targets[@]}"; do
            log "cargo build --release --target $target"
            rustup target add "$target" >/dev/null
            cargo build --release --locked -p app --bin crabtrap --target "$target" \
                --manifest-path "$ROOT/Cargo.toml"
        done
    fi
    BINARY="$DIST/crabtrap-bin"
    if $universal; then
        lipo -create -output "$BINARY" \
            "$ROOT/target/$ARM_TARGET/release/crabtrap" \
            "$ROOT/target/$X86_TARGET/release/crabtrap"
        ARCH_LABEL="universal"
    else
        cp "$ROOT/target/$ARM_TARGET/release/crabtrap" "$BINARY"
        ARCH_LABEL="arm64"
    fi
}

assemble_app() {
    APP="$DIST/crabtrap.app"
    log "assemble $APP"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    mv "$BINARY" "$APP/Contents/MacOS/crabtrap"
    cp "$PACKAGING/icon/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
    sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" \
        "$PACKAGING/Info.plist.in" >"$APP/Contents/Info.plist"
    plutil -lint "$APP/Contents/Info.plist" >/dev/null
}

sign_app() {
    log "codesign (identity: $SIGN_IDENTITY)"
    local flags=(--force --sign "$SIGN_IDENTITY" --entitlements "$PACKAGING/entitlements.plist")
    # Hardened runtime и timestamp нужны только для нотаризации (Developer ID).
    [[ "$SIGN_IDENTITY" != "-" ]] && flags+=(--options runtime --timestamp)
    codesign "${flags[@]}" "$APP"
    codesign --verify --strict --verbose=2 "$APP"
}

make_dmg() {
    DMG="$DIST/crabtrap-$VERSION-macos-$ARCH_LABEL.dmg"
    log "create $DMG"
    local staging
    staging="$(mktemp -d)"
    cp -R "$APP" "$staging/"
    ln -s /Applications "$staging/Applications"
    rm -f "$DMG"
    hdiutil create -volname "crabtrap $VERSION" -srcfolder "$staging" \
        -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
    rm -rf "$staging"
    [[ "$SIGN_IDENTITY" != "-" ]] && codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG"
    return 0
}

notarize_dmg() {
    if [[ "$SIGN_IDENTITY" == "-" ]]; then
        log "ad-hoc signature: notarization skipped"
        return
    fi
    if [[ -z "${APPLE_API_KEY_PATH:-}" || -z "${APPLE_API_KEY_ID:-}" || -z "${APPLE_API_ISSUER:-}" ]]; then
        log "no App Store Connect API key: notarization skipped"
        return
    fi
    log "notarytool submit (waits for Apple)"
    xcrun notarytool submit "$DMG" --wait \
        --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER"
    xcrun stapler staple "$DMG"
}

main() {
    mkdir -p "$DIST"
    VERSION="$(workspace_version)"
    log "crabtrap $VERSION"
    build_binary
    assemble_app
    sign_app
    make_dmg
    notarize_dmg
    (cd "$DIST" && shasum -a 256 "$(basename "$DMG")" >"$(basename "$DMG").sha256")
    log "done: $DMG"
}

main
