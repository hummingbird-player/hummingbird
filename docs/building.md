# Building

## Dependencies
In order to build Hummingbird, you'll need the latest Rust, Cargo, and Git. You'll also need a few dependencies depending on what platform you're building for.

### Windows
You'll need everything in the [Zed compilation instructions](https://zed.dev/docs/development/windows).

### macOS
You need an up-to-date version of Xcode, as well as `cmake` in your PATH.

### Linux
On Linux, you'll need the development headers for several packages. The following package lists are provided on a best-effort basis and may not always be up to date.

- **Fedora/CentOS/EL**
  ```sh
  sudo dnf install libxkbcommon-devel libxkbcommon-x11-devel alsa-lib-devel make gcc perl-core pcre-devel zlib-devel libX11-devel wayland-devel openssl-devel libxcb-devel fontconfig-devel pipewire-devel clang cmake dbus-devel glib-devel g++
  ```
- **Ubuntu**
  ```sh
  sudo apt update
  sudo apt install libasound2-dev pkg-config libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev build-essential libfontconfig1-dev libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev clang cmake libssl-dev libwayland-dev
  ```
  
### NixOS / Nix / Nix (darwin)
A Nix flake is provided in the repository. A build cache is provided via [Cachix](https://hummingbird.cachix.org/), for `x86_64-linux`, `aarch64-linux`, and `aarch64-darwin`, if you wish to use it.

## Environment
Enabling last.fm support requires setting `LASTFM_API_KEY` and `LASTFM_API_SECRET` in your environment variables. This can be done at compile time (in which they will be embedded in the binary), or at runtime (in which they will be read from the environment).

## Offline Builds/Libre-only Services
Some users may wish to prevent Hummingbird from accessing the internet, or prevent it from accessing proprietary online services (like last.fm). Hummingbird's online services can be disabled using cargo features.

To disable all online services, use the `--no-default-features` flag when building:

```sh
cargo build --release --no-default-features
```

To enable only libre services, use the `--features libre-services` flag (with `--no-default-features`) when building:

```sh
cargo build --release --features libre-services --no-default-features
```

## Building
```sh
git clone https://git.mailliw.org/hummingbird/hummingbird
cd hummingbird

# debug mode will result in noticeable slowdown in most cases
cargo build --release
```
