# Hummingbird

![screenshot](docs/screenshot.png)

Hummingbird is a modern music player written in Rust using GPUI, designed to be
performant and lightweight while maximizing extensibility and maintaining a high
design standard.

# Features
- Fully native application with no web component
- FLAC, MP3, OGG Vorbis, Opus, AAC and WAV playback
- Linux, macOS and Windows support
- SQLite-backed library
- Theming with hot reload
- Scrobbling (last.fm) support
- Fuzzy-find search
- ReplayGain
- Desktop integration
- Playlists
- Lyrics
- Discord RPC

## Planned Features
- WASM Extension support:
  - Codecs
  - Scrobble services
  - Metadata services
- Improved library management

# Usage

## Stable Builds

### Windows, macOS and Linux AppImage
Binaries for these platforms are officially available for 64-bit ARM and Intel architectures.
See the [latest release](https://github.com/hummingbird-player/hummingbird/releases/latest) for downloads.

### Fedora
Fedora users can install Hummingbird using the `william341/hummingbird` COPR:
```bash
sudo dnf copr enable william341/hummingbird
sudo dnf install hummingbird
```

### Arch
Arch users can install Hummingbird using the `hummingbird-player` AUR package:
```bash
paru -S hummingbird-player
```

## NixOS
A flake is available for NixOS users. Work towards upstreaming Hummingbird into nixpkgs is
currently ongoing.

## Unstable Builds
The latest commit is built using Github Actions and uploaded to the
[latest](https://github.com/143mailliw/hummingbird/releases/tag/latest) tag
automatically. Windows installers aren't available for unstable builds.

Official builds are signed with `minisign`. The signature is attached to the release and must be downloaded seperately and placed next to the binary you are attempting to verify. Verify with:
```
minisign -Vm <file> -P RWTVGbNhJ/77g9Dm280SNcfxaPz118Hgg8vI55tFX83sIMiObZuxpDyV
```

## Building
For more detailed instructions, see the [Building](docs/building.md) documentation.

```sh
# install relevant devel packages for xcb-common, x11, wayland, openssl, and pulseaudio if on Linux
git clone https://github.com/143mailliw/hummingbird
cd hummingbird

# last.fm api keys must be set in the environment for scrobbling to work
# these can be obtained from https://www.last.fm/api/account/create
# you can also set these in a .env file in the root of the project
#
# Hummingbird will still build without these keys, but scrobbling will be disabled
export LASTFM_API_KEY="your key"
export LASTFM_API_SECRET="your secret"

# debug mode will result in noticable slowdown in some cases
cargo build --release
```

# Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md).
