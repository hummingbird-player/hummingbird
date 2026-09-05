# Library sources

Subsonic sources share Hummingbird's library database, search, playlists, queue,
metadata views, and audio engine. Connection creation is currently hidden behind
`src/sources.rs::SOURCE_UI_READY` while the implementation is being simplified.

## Connection and playback behavior

Connections live in Settings > Services > Music libraries. Each has its own name,
account, folder selection, refresh policy, quality policy, cache budget, and
reporting preferences. Editing uses a draft; saving applies the configuration.
Passwords/API keys use the platform credential store or explicit session-only
storage. Settings contain credential references, not secrets. HTTP requires
explicit opt-in; cross-origin redirects and certificate bypass are not enabled.

Original is the default quality. Automatic may retry a supported encoding after a
decoder rejection; explicit presets request a format/bitrate. The player displays
the actual decoded format. Server transcoding and time-offset support are discovered
by the Subsonic adapter. Unsupported seeks produce a limitation/error instead of
pretending a stream's byte offset is its playback time.

Completed downloads remain remote tracks. Disabling a connection stops its network
work and retains the indexed library, local playlist membership, and completed
copies. Removing configuration also retains that data; purging it is a separate
confirmation. Account replacement creates a new source identity unless the user
explicitly confirms the same library. Old work cannot transfer to another account.

Local likes and mixed local playlists work independently of server write permissions.
M3U exports retain local paths and use credential-free `hummingbird://track`
references for remote entries. Those references are for Hummingbird round trips;
other players need actual downloaded files. Advanced server playlist editing,
ratings write-back, bookmarks, and recommendations are not advertised.

## Ownership and concurrency

- `SourceId` identifies a configured account, and `TrackRef` combines it with a
  locator. Remote locators are opaque IDs; only local locations expose filesystem
  paths. SQL uniqueness is `(source, location)`, and local scanner operations are
  explicitly scoped to the reserved local source.
- `sources/backend.rs` defines protocol-independent catalog/media operations.
  The adapter handles authenticated HTTP and translates server responses. The
  host owns database writes, credentials, scheduling, cancellation, and cache policy.
- `sources/service.rs`, `registry.rs`, and `sync.rs` manage connection jobs and
  imports. Configuration generations reject stale work. Pages commit in bounded
  transactions; incomplete snapshots cannot delete catalog entries. UI views read
  the database and cached status, not the network.
- `media/input.rs` separates byte input from codec selection. Local files keep their
  direct path. Remote input uses a bounded disk window on a decoder worker, with
  bounded PCM delivery to the existing engine. Network waits cannot monopolize
  playback controls or run in the audio callback.
- `sources/cache.rs` owns completed copies and active pins. Partial data is not
  offline-playable. Artwork and lyrics use separate bounded, lazy caches; account
  identity and cancellation apply to those results too.

These are Rust interfaces within one program. Playback events do not need wire
versions or serialization to cross threads. A future WASM adapter can translate
owned records and implement the existing operations without receiving database,
UI, or decoder objects. ABI versioning and plugin runtime policy belong to that
adapter. Persisted queue formats and server-advertised extension versions have
real compatibility requirements and remain versioned.

## Playback reporting

Playback sessions carry source identity, original start time, and cumulative
rendered listening time. Seeking, pausing, buffering, and prefetch do not count as
listening. A repeated track has a new session. MMBS delivers events in order to
service workers; source-server reporting never matches local tracks by title.

Qualified source listens are persisted before sending. Retry and retention are
bounded, and offline listens retain their original timestamps. Delivery is
**at least once**: a server accepting a request but losing its response can count
a retry again. Disabling statistics stops new reports and pauses pending sends;
clearing the queue and account replacement invalidate unsent work.

Source-server statistics and direct Last.fm/ListenBrainz forwarding have separate
controls. A server may itself forward listens, so users can exclude a source from
either direct integration without changing the global preference. Optional server
playback-state updates use `ignoreScrobble=true` to avoid a second counting path.
Gonic 0.22.0 has a version-specific adapter workaround for its now-playing and
batch-scrobble behavior; qualified single submissions remain supported.

## Verification

See [integration testing](subsonic-acceptance.md) for reproducible fixture setup and
commands. Core regression coverage includes populated migrations, local scan
isolation, mixed queue restore, cancellation, bounded input, cache retirement,
account changes, and rendered-time reporting. The rough branch preserves the
larger experiment record. Those historical measurements do not substitute for
checking the final simplified implementation, nor prove cross-platform runtime
behavior or sample-exact lossy transitions.
