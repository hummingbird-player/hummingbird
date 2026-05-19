#!/bin/sh
set -eu

TARGET=aarch64-unknown-linux-gnu
APPIMAGE=hummingbird-aarch64.AppImage
TARGET_DIR=${CARGO_TARGET_DIR:-target}
APPDIR=$TARGET_DIR/bundle/$TARGET/release/appdir

cargo install --git https://github.com/vicr123/contemporary-rs.git cargo-cntp-bundle
cargo install --git https://github.com/vicr123/contemporary-rs.git cargo-cntp-deploy

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig
export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

cargo build --verbose --release -F update --target "$TARGET"
cargo cntp-bundle --no-open --verbose --target "$TARGET"

cp -L /usr/lib/aarch64-linux-gnu/libxkbcommon.so.0 "$APPDIR/usr/lib/"
cp -L /usr/lib/aarch64-linux-gnu/libxkbcommon-x11.so.0 "$APPDIR/usr/lib/"
cp -L /usr/lib/aarch64-linux-gnu/libxcb-xkb.so.1 "$APPDIR/usr/lib/"

find "$APPDIR" -type f -exec file {} \; | tee appdir-files.txt
if grep -E 'ELF .*x86-64' appdir-files.txt; then
  echo "Found x86-64 ELF files in aarch64 AppDir" >&2
  exit 1
fi

ARCH=aarch64 cargo cntp-deploy \
  --no-open \
  --verbose \
  --target "$TARGET" \
  --output-file "$APPIMAGE"
