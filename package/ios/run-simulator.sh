#!/usr/bin/env bash
# Builds Hummingbird.app for the iOS simulator, boots a simulator and opens Simulator.app if needed,
# then installs and launches the app with its output streamed to this terminal (Ctrl-C stops it).
#
# Environment:
#   HUMMINGBIRD_SIMULATOR  simulator UDID or name to use (default: the first booted simulator, or
#                          else the first available iPhone simulator)
#
# Extra arguments are passed to `cargo build` (e.g. --release).
set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
target_directory="${CARGO_TARGET_DIR:-"$repository_root/target"}"
bundle_id="$(plutil -extract CFBundleIdentifier raw "$repository_root/package/ios/Info.plist")"
profile_directory="debug"
for argument in "$@"; do
    [[ "$argument" == "--release" ]] && profile_directory="release"
done

uuid_pattern='[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}'

simulator="${HUMMINGBIRD_SIMULATOR:-}"
if [[ -z "$simulator" ]]; then
    simulator="$(xcrun simctl list devices booted | grep -Eo "$uuid_pattern" | head -n 1 || true)"
fi
if [[ -z "$simulator" ]]; then
    simulator="$(xcrun simctl list devices available iPhone | grep -Eo "$uuid_pattern" | head -n 1 || true)"
fi
[[ -n "$simulator" ]] || { echo "No iOS simulator found; create one in Xcode" >&2; exit 1; }

"$repository_root/package/ios/build-simulator.sh" "$@"
app_directory="$target_directory/aarch64-apple-ios-sim/$profile_directory/Hummingbird.app"

echo "Booting simulator: $simulator"
xcrun simctl bootstatus "$simulator" -b > /dev/null

# Xcode 27 replaced Simulator.app with DeviceHub.app; open whichever is installed.
if ! pgrep -xq Simulator && ! pgrep -xq DeviceHub; then
    open -b com.apple.iphonesimulator 2> /dev/null \
        || open -b com.apple.dt.Devices 2> /dev/null \
        || echo "Neither Simulator.app nor DeviceHub.app found; the simulator is running headless" >&2
fi

xcrun simctl install "$simulator" "$app_directory"
echo "Launching $bundle_id"
xcrun simctl launch --console-pty --terminate-running-process "$simulator" "$bundle_id"
