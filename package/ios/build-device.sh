#!/usr/bin/env bash
# Builds, signs and (optionally) installs Hummingbird.app on a physical iOS device.
#
# Needs an iOS development provisioning profile that covers the bundle ID and the device, plus a
# matching "Apple Development" certificate in the keychain. Building any app to the device once
# from Xcode with automatic signing creates both.
#
# Environment:
#   HUMMINGBIRD_PROFILE   path to the .mobileprovision to embed (default: newest iOS profile found
#                         in Xcode's profile directory whose app ID covers the bundle ID)
#   HUMMINGBIRD_IDENTITY  SHA-1 of the signing certificate (default: first keychain identity that
#                         the profile allows)
#   HUMMINGBIRD_DEVICE    device identifier for `xcrun devicectl`; when set, the app is installed
#
# Extra arguments are passed to `cargo build` (e.g. --release).
set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
target_directory="${CARGO_TARGET_DIR:-"$repository_root/target"}"
info_plist="$repository_root/package/ios/Info.plist"
bundle_id="$(plutil -extract CFBundleIdentifier raw "$info_plist")"
profile_directory="debug"
for argument in "$@"; do
    [[ "$argument" == "--release" ]] && profile_directory="release"
done

work_directory="$(mktemp -d)"
trap 'rm -rf "$work_directory"' EXIT

decode_profile() {
    security cms -D -i "$1" > "$work_directory/profile.plist" 2>/dev/null
}

profile_covers_bundle() {
    local app_id team
    app_id="$(plutil -extract Entitlements.application-identifier raw "$work_directory/profile.plist")"
    team="$(plutil -extract TeamIdentifier.0 raw "$work_directory/profile.plist")"
    [[ "$app_id" == "$team.$bundle_id" || "$app_id" == "$team.*" ]]
}

profile="${HUMMINGBIRD_PROFILE:-}"
if [[ -z "$profile" ]]; then
    newest=0
    for candidate in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"/*.mobileprovision \
        "$HOME/Library/MobileDevice/Provisioning Profiles"/*.mobileprovision; do
        [[ -f "$candidate" ]] || continue
        decode_profile "$candidate" || continue
        [[ "$(plutil -extract Platform.0 raw "$work_directory/profile.plist" 2>/dev/null)" == "iOS" ]] || continue
        profile_covers_bundle || continue
        modified="$(stat -f %m "$candidate")"
        if (( modified > newest )); then
            newest="$modified"
            profile="$candidate"
        fi
    done
fi
[[ -n "$profile" ]] || { echo "No iOS provisioning profile covers $bundle_id" >&2; exit 1; }
decode_profile "$profile"
team="$(plutil -extract TeamIdentifier.0 raw "$work_directory/profile.plist")"
echo "Using profile: $(plutil -extract Name raw "$work_directory/profile.plist") ($profile)"

identity="${HUMMINGBIRD_IDENTITY:-}"
if [[ -z "$identity" ]]; then
    count="$(plutil -extract DeveloperCertificates raw "$work_directory/profile.plist")"
    allowed=""
    for ((index = 0; index < count; index++)); do
        allowed+="$(plutil -extract "DeveloperCertificates.$index" raw "$work_directory/profile.plist" \
            | base64 -d | shasum -a 1 | awk '{print toupper($1)}') "
    done
    while read -r hash; do
        if [[ " $allowed " == *" $hash "* ]]; then
            identity="$hash"
            break
        fi
    done < <(security find-identity -v -p codesigning | awk '/Apple Development/ {print $2}')
fi
[[ -n "$identity" ]] || { echo "No keychain identity matches the profile's certificates" >&2; exit 1; }
echo "Signing with: $identity"

cd "$repository_root"
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo build --target aarch64-apple-ios "$@"

build_directory="$target_directory/aarch64-apple-ios/$profile_directory"
app_directory="$build_directory/Hummingbird.app"
rm -rf "$app_directory"
mkdir -p "$app_directory"
cp "$build_directory/hummingbird" "$app_directory/hummingbird"
cp "$info_plist" "$app_directory/Info.plist"
cp "$profile" "$app_directory/embedded.mobileprovision"

cat > "$work_directory/entitlements.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>application-identifier</key>
	<string>$team.$bundle_id</string>
	<key>com.apple.developer.team-identifier</key>
	<string>$team</string>
	<key>get-task-allow</key>
	<true/>
</dict>
</plist>
EOF

codesign --force --sign "$identity" --entitlements "$work_directory/entitlements.plist" \
    --timestamp=none "$app_directory"
codesign --verify --strict "$app_directory"
printf 'Built device app: %s\n' "$app_directory"

if [[ -n "${HUMMINGBIRD_DEVICE:-}" ]]; then
    xcrun devicectl device install app --device "$HUMMINGBIRD_DEVICE" "$app_directory"
fi
