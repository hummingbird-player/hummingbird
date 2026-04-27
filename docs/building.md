# Building

## Dependencies
In order to build Hummingbird, you'll need Rust (to my knowledge, >= 1.88, but later is always better), Cargo, and Git. You'll also need a few dependencies depending on what platform you're building for.

### Windows
You'll need everything in the [Zed compilation instructions](https://zed.dev/docs/development/windows).

### macOS
You should just need Rust and Xcode.

### Linux
On Linux, you'll need the development headers for several packages. The following package lists are provided on a best-effort basis and may not always be up to date.

- **Fedora/CentOS/EL**
  ```sh
  sudo dnf install libxkbcommon-devel libxkbcommon-x11-devel alsa-lib-devel make gcc perl-core pcre-devel zlib-devel libX11-devel wayland-devel pulseaudio-libs-devel openssl-devel libxcb-devel pulseaudio-libs-devel fontconfig-devel pipewire-devel
  ```
- **Ubuntu**
  ```sh
  sudo apt update
  sudo apt install libasound2-dev pkg-config libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libpulse-dev build-essential libfontconfig1-dev libpipewire-0.3-dev libspa-0.2-dev
  ```
  
### NixOS / Nix / Nix (darwin)
Some members of the community have provided a Nix flake. We try to keep it fairly up to date, but if you have a problem please do report an issue.

## Environment
If you wish to use last.fm support with your build, you'll have to set `LASTFM_API_KEY` and `LASTFM_API_SECRET` in either your environment variables or in your `.env` file. If you don't set these variables, Hummingbird will still build, but last.fm support will be disabled, and you'll get a warning in the logs.

## Building
```sh
git clone https://github.com/143mailliw/hummingbird
cd hummingbird

# debug mode will result in noticable slowdown in some cases
cargo build --release
```
