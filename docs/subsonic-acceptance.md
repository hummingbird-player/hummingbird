# Subsonic integration testing

Use disposable loopback servers and the generated fixture library. The integration
test exercises the real transport, importer, decoder worker, cache, artwork,
lyrics, and reporting. It modifies fixture play counts and must not target a real
account. The test rejects endpoints outside `127.0.0.1`.

## Disposable server setup

Use official Linux binaries for [Navidrome](https://github.com/navidrome/navidrome/releases)
and [Gonic](https://github.com/sentriz/gonic/releases). Verify release checksums.
Neither server needs a system installation; both use the installed `ffmpeg`.
Generate a new fixture directory:

```sh
python3 scripts/subsonic-fixture.py /tmp/hummingbird-subsonic-fixture
```

Start Navidrome with its generated configuration (replace binary paths as needed):

```sh
/path/to/navidrome --configfile /tmp/hummingbird-subsonic-fixture/navidrome.toml
```

After the server initializes its database, create the disposable admin account in
another terminal:

```sh
/path/to/navidrome user create --configfile /tmp/hummingbird-subsonic-fixture/navidrome.toml --username hb-fixture --admin
```

Enter `hb-fixture-test-only` at both password prompts. Start Gonic using its
disposable default `admin` account:

```sh
/path/to/gonic \
  -music-path /tmp/hummingbird-subsonic-fixture/music \
  -cache-path /tmp/hummingbird-subsonic-fixture/gonic/cache \
  -podcast-path /tmp/hummingbird-subsonic-fixture/gonic/podcasts \
  -playlists-path /tmp/hummingbird-subsonic-fixture/gonic/playlists \
  -db-path /tmp/hummingbird-subsonic-fixture/gonic/gonic.db \
  -listen-addr 127.0.0.1:14747 \
  -scan-at-start-enabled -http-log=false
```

These credentials are only for the loopback test servers. The test rejects other
hosts. Navidrome's base URL covers proxy-prefix handling; Gonic's `proxy-prefix`
option only generates proxy-facing links and requires an actual prefix-stripping
proxy, so this direct test uses Gonic's root endpoint. The test
requires the generated song title before issuing reporting requests.
It deliberately changes the fixture song's play count. It must not be pointed at
a real account. Stop both servers with Ctrl-C after testing.

## Run

```sh
HUMMINGBIRD_TEST_SERVERS=/tmp/hummingbird-subsonic-fixture/endpoints.json \
  cargo test --bin hummingbird real_server_catalog_stream_seek_cache_assets_and_reporting \
  -- --ignored --nocapture
```

Use the normal platform build environment. Local socket access is required.
The test prints discovered server versions/capabilities, catalog counts,
preparation timings, actual decoder formats, lyric availability, observed play
counts, and cached playback results. Missing optional lyrics or play-count fields
are reported; they must not be described as successful verification of those
features. Timing output is a small-fixture diagnostic, not a performance baseline.

For native acceptance, exercise a mixed local/remote queue, pause/seek/skip,
restart, disabled-source cached playback, interrupted import/playback, and account
replacement. Confirm that local playlist membership and unrelated sources remain
intact. Inspect actual server counts for qualified and offline-reconnected listens;
prefetch and download alone must not increment them.


## Other checks

Run `cargo test --bin hummingbird` and all-target checks without default features,
with `online`, and with `libre-services`. Loopback HTTP tests need local socket
access. The ignored `large_catalog_import_refresh_and_reader_latency` fixture
measures a 50,000-track import and concurrent SQLite reads; it is not a UI benchmark.

The rough branch preserves the detailed experiment record. Linux workflow checks
used Navidrome 0.63.2 and Gonic 0.22.0. Native Linux measurements do not establish
Windows/macOS runtime compatibility or sample-exact transcoder transitions.
