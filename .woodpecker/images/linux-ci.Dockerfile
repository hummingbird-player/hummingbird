FROM rust:1.97.1-bookworm

ENV DEBIAN_FRONTEND=noninteractive
ENV APPIMAGE_EXTRACT_AND_RUN=1
ENV CARGO_TERM_COLOR=always
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CARGO_TARGET_DIR=/opt/hummingbird/target

RUN dpkg --add-architecture arm64 \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        desktop-file-utils \
        file \
        g++-aarch64-linux-gnu \
        gcc-aarch64-linux-gnu \
        git \
        jq \
        libasound2-dev \
        libasound2-dev:arm64 \
        libclang-dev \
        libfontconfig1-dev \
        libfontconfig1-dev:arm64 \
        libpipewire-0.3-dev \
        libpipewire-0.3-dev:arm64 \
        libpulse-dev \
        libpulse-dev:arm64 \
        libspa-0.2-dev \
        libspa-0.2-dev:arm64 \
        libx11-xcb-dev \
        libx11-xcb-dev:arm64 \
        libxkbcommon-dev \
        libxkbcommon-dev:arm64 \
        libxkbcommon-x11-dev \
        libxkbcommon-x11-dev:arm64 \
        minisign \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install stable \
    && rustup component add rustfmt clippy \
    && rustup component add --toolchain stable rustfmt clippy \
    && rustup target add aarch64-unknown-linux-gnu \
    && rustup target add --toolchain stable aarch64-unknown-linux-gnu

RUN cargo install --git https://github.com/vicr123/contemporary-rs.git cargo-cntp-bundle \
    && cargo install --git https://github.com/vicr123/contemporary-rs.git cargo-cntp-deploy

WORKDIR /opt/hummingbird
COPY Cargo.toml Cargo.lock ./
COPY crates/gpui-unofficial-shim crates/gpui-unofficial-shim
RUN mkdir -p src \
    && echo '// placeholder for cargo fetch' > src/lib.rs \
    && cargo fetch --locked \
    && rm -rf /opt/hummingbird

# Pre-build all release dependencies for host (amd64)
WORKDIR /opt/hummingbird
COPY Cargo.toml Cargo.lock ./
COPY crates/gpui-unofficial-shim crates/gpui-unofficial-shim
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked -F update \
    && cargo clean -p hummingbird \
    && rm -rf src

# Pre-build all release dependencies for cross-target (arm64)
RUN export PKG_CONFIG_ALLOW_CROSS=1 \
    && export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig \
    && export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    && mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked -F update --target aarch64-unknown-linux-gnu \
    && cargo clean -p hummingbird --target aarch64-unknown-linux-gnu \
    && rm -rf src

# Pre-build debug dependencies for cargo test
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --locked -F update \
    && cargo clean -p hummingbird \
    && rm -rf src

WORKDIR /woodpecker/src
