#!/usr/bin/env bash
# Builds "HT Plugin Manager.app" in target/ and signs it with the
# Developer ID the plugins use ("Developer ID Application: Jonas Grunau (JQ4STVWTQ3)").
#   scripts/bundle.sh                    build + sign (native architecture)
#   scripts/bundle.sh --universal        … arm64 + x86_64 in one binary
#   scripts/bundle.sh --dmg              … and HT-Plugin-Manager-<v>-macOS.dmg (signed)
#   scripts/bundle.sh --notarize         … and notarize + staple (the DMG with --dmg, else the app)
#   scripts/bundle.sh --install          … and copy the app to /Applications
#
# Signing identity: HTPM_SIGN_IDENTITY (default: the Developer ID above,
# from the login keychain; "-" signs ad hoc, for builds without the certificate).
# Notary credentials: NOTARY_APPLE_ID + NOTARY_PASSWORD + TEAM_ID when set (CI);
# otherwise the notarytool keychain profile "ht-plugin-manager-notary", created on the
# first --notarize from 1Password (the items the plugin release pipelines read).
# HTPM_GITHUB_CLIENT_ID, if set, overrides the OAuth client ID built into src/config.rs.
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY=${HTPM_SIGN_IDENTITY:-"Developer ID Application: Jonas Grunau (JQ4STVWTQ3)"}
TEAM=${TEAM_ID:-JQ4STVWTQ3}
PROFILE=ht-plugin-manager-notary
OP_ACCOUNT=${OP_ACCOUNT:-my.1password.com}
NAME="HT Plugin Manager"
OUT=${HTPM_OUT:-target}

universal=0 dmg=0 notarize=0 install=0
for arg in "$@"; do
    case $arg in
        --universal) universal=1 ;;
        --dmg) dmg=1 ;;
        --notarize) notarize=1 ;;
        --install) install=1 ;;
        *) echo "unknown option $arg" >&2; exit 2 ;;
    esac
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p "$OUT"

if (( universal )); then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    binary=target/ht-plugin-manager-universal
    lipo -create -output "$binary" target/aarch64-apple-darwin/release/ht-plugin-manager target/x86_64-apple-darwin/release/ht-plugin-manager
else
    cargo build --release
    binary=target/release/ht-plugin-manager
fi

app="$OUT/$NAME.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/ht-plugin-manager"
cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$NAME</string>
    <key>CFBundleDisplayName</key><string>$NAME</string>
    <key>CFBundleIdentifier</key><string>de.grunau.ht-plugin-manager</string>
    <key>CFBundleExecutable</key><string>ht-plugin-manager</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>Jonas Grunau</string>
</dict>
</plist>
EOF

sign=(--force --sign "$IDENTITY")
[[ "$IDENTITY" == - ]] || sign+=(--options runtime --timestamp)
codesign "${sign[@]}" "$app"
codesign --verify --strict --verbose=1 "$app"
echo "signed: $app ($version, ${IDENTITY%% (*})"

notary() {
    if [[ -n "${NOTARY_APPLE_ID:-}" ]]; then
        echo --apple-id "$NOTARY_APPLE_ID" --password "$NOTARY_PASSWORD" --team-id "$TEAM"
        return
    fi
    if ! xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
        command -v op >/dev/null || { echo "1Password CLI (op) is missing: brew install 1password-cli" >&2; exit 1; }
        xcrun notarytool store-credentials "$PROFILE" --team-id "$TEAM" \
            --apple-id "$(op read 'op://Private/Apple/username' --account "$OP_ACCOUNT")" \
            --password "$(op read 'op://Private/PsyBox Signing/App Specific Password' --account "$OP_ACCOUNT")" >&2
    fi
    echo --keychain-profile "$PROFILE"
}

submit() {
    local file=$1 auth result
    read -r -a auth <<<"$(notary)"
    result=$(xcrun notarytool submit "$file" "${auth[@]}" --wait --timeout 1h --output-format json)
    echo "$result"
    if [[ $(/usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' <<<"$result") != Accepted ]]; then
        xcrun notarytool log "$(/usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$result")" "${auth[@]}"
        exit 1
    fi
}

if (( dmg )); then
    out="$OUT/HT-Plugin-Manager-$version-macOS.dmg"
    stage=$(mktemp -d)
    trap 'rm -rf "$stage"' EXIT
    ditto "$app" "$stage/$NAME.app"
    ln -s /Applications "$stage/Applications"
    rm -f "$out"
    # hdiutil now and then fails with "Resource busy" on CI runners; try again.
    for attempt in 1 2 3; do
        hdiutil create -volname "$NAME" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$out" && break
        (( attempt == 3 )) && exit 1
        sleep 5
    done
    if [[ "$IDENTITY" != - ]]; then
        codesign --force --sign "$IDENTITY" --timestamp "$out"
    fi
    if (( notarize )); then
        submit "$out"
        xcrun stapler staple "$out"
        spctl --assess --type open --context context:primary-signature --verbose "$out"
    fi
    echo "dmg: $out"
elif (( notarize )); then
    zip="$OUT/HT-Plugin-Manager-$version.zip"
    ditto -c -k --keepParent "$app" "$zip"
    submit "$zip"
    xcrun stapler staple "$app"
    spctl --assess --type execute --verbose "$app"
    ditto -c -k --keepParent "$app" "$zip"
    echo "notarized: $zip"
fi

if (( install )); then
    rm -rf "/Applications/$NAME.app"
    ditto "$app" "/Applications/$NAME.app"
    echo "installed: /Applications/$NAME.app"
fi
