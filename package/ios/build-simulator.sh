#!/usr/bin/env bash
# Builds an ad-hoc signed Hummingbird.app for the iOS simulator.
# Extra arguments are passed to `cargo build` (e.g. --release).
set -euo pipefail

repository_root="$(git rev-parse --show-toplevel)"
target_directory="${CARGO_TARGET_DIR:-"$repository_root/target"}"
profile_directory="debug"
for argument in "$@"; do
    [[ "$argument" == "--release" ]] && profile_directory="release"
done

cd "$repository_root"
IPHONEOS_DEPLOYMENT_TARGET=15.0 \
    cargo build --target aarch64-apple-ios-sim "$@"

build_directory="$target_directory/aarch64-apple-ios-sim/$profile_directory"
app_directory="$build_directory/Hummingbird.app"
mkdir -p "$app_directory"
cp "$build_directory/hummingbird" "$app_directory/hummingbird"
cp "$repository_root/package/ios/Info.plist" "$app_directory/Info.plist"
codesign --force --sign - "$app_directory"
printf 'Built simulator app: %s\n' "$app_directory"
