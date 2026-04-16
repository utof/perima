# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While on `0.x`, per the Cargo / Rust ecosystem convention, **MINOR version bumps
may include breaking API changes**; PATCH releases are additive/fix-only. The
project will remain on `0.x` until an API stability commitment is made — no fixed
roadmap milestone triggers `1.0.0`.

## [Unreleased]

## [0.5.1] — 2026-04-16

Desktop tag UI layer on top of the v0.5.0 tag backend.

### Added

- **Tauri tag commands** (`perima-desktop`): `list_tags`, `attach_tag`,
  `detach_tag`, `list_files_with_tags`. Two-query merge (metadata +
  tags_for_hashes) with a WHY comment documenting the WAL race.
- **`TagPayload` + `FileWithTagsPayload`** wire types in `payloads.rs`;
  `FileWithTagsPayload` composes (not extends) `FileWithMetadataPayload`.
- **TS `Tag` / `FileWithTags` types** + `api.ts` wrappers (`listTags`,
  `attachTag`, `detachTag`, `listFilesWithTags`) following the existing
  `fromInvoke` / neverthrow pattern.
- **`TagChip` component** — colored pill with optional remove button.
  Color index computed via byte-sum mod 12 (blake3 not available in TS
  bundle; intentional deviation from spec; WHY comment explains).
- **`TagSidebar` component** — "All" + per-tag rows with attachment counts
  and `aria-pressed` accessibility. `totalCount` prop shows unfiltered
  file count next to "All". Active-state count badge uses `text-blue-200`
  for legibility on the blue background.
- **`FileTable` / `FileGrid` tag rendering** — up to 3 `TagChip` instances
  per row/tile with `+N` overflow badge.
- **`App.tsx` client-side tag filter** — `selectedTagId: string | null`
  state; `<TagSidebar>` shown when tags list is non-empty; `visibleFiles`
  filters `files` by tag id. WHY comments explain single-select deferral
  and 100-row cap effect on displayed counts.

## [0.5.0] — 2026-04-16

First user-facing organization primitive: **tag-based file labeling**.
Backend-only release; desktop UI lands in v0.5.1.

### Added

- **Tag domain type + normalization** (`perima-core`).
  `Tag { id, name, first_seen }` value type; `normalize()` applies
  trim → NFC → lowercase with `MAX_TAG_LEN = 64` guard.
  `CoreError::InvalidTag` variant for validation failures.
  `TagRepository` trait with 8 methods (`upsert_tag`, `delete_tag`,
  `attach`, `detach`, `list_tags`, `tags_for_hashes`,
  `files_with_tag`, `count_files_for_tag`).
- **V005 migration + `SqliteTagRepository`** (`perima-db`).
  `tags` and `file_tags` tables (CRDT-compliant: soft deletes,
  `updated_at` + `device_id` on every mutable row, no UNIQUE on
  mutable columns, no FK cascades). Content-addressed via
  `blake3_hash`. Composite `(blake3_hash, tag_id)` covering index +
  reverse `tag_id` index. `tags_for_hashes` batches via
  `params_from_iter`; short-circuits on empty slice.
  14 new tests including Barrier-driven concurrent upsert.
- **CLI tag subcommand** (`perima`).
  `perima tag add <path> <tags...>` (1+ required),
  `perima tag rm <path> <tag>`,
  `perima tag ls [--json]` with per-tag file counts.
  `perima ls --tag <name>` filters the file listing to tagged files.
  3 new integration tests.

[0.5.0]: https://github.com/utof/perima/releases/tag/v0.5.0

## [0.4.3] — 2026-04-16

Single-blocker follow-up to the v0.4.2 hotfix. No new features; one
correctness fix and one CHANGELOG link repair.

### Fixed

- **Tauri asset-protocol scope now matches the runtime data dir.**
  v0.4.2 narrowed `assetProtocol.scope` to
  `$APPDATA/perima/thumbnails/**` but the runtime still resolved
  `data_dir` via `directories::ProjectDirs` which produces a
  different subtree than Tauri's `$APPDATA` (`directories` uses
  `~/.local/share/perima` on Linux; Tauri uses
  `~/.local/share/dev.perima.desktop`, based on the bundle
  identifier). Every `convertFileSrc(thumbnail_path)` returned 404 —
  the grid view showed broken placeholder tiles for every image on
  every platform. Fix: `perima_desktop::run` now resolves `data_dir`
  via `app.path().app_data_dir()` inside `.setup()` (new
  `Config::resolve_with_app_data_dir` entry point), with `data_dir`
  set to `<app_data_dir>/perima` so the existing scope literal
  matches. New regression test pins
  `thumb_root.starts_with(app_data_dir)` AND
  `thumb_root.ends_with("perima/thumbnails")`.
- **CHANGELOG `[Unreleased]` compare link** was pointing at
  `v0.4.1...HEAD` after the v0.4.2 release; corrected to
  `v0.4.3...HEAD`.

### Notes

- Runtime verification of the scope fix is deferred to user testing —
  no display available on the dev / CI machine. The regression test
  pins the path invariant but cannot exercise `convertFileSrc` end-
  to-end without a WebView.
- Follow-up for v0.4.0 / v0.4.1 upgraders: the V004 backfill (v0.4.2)
  flipped pre-existing `thumbnail_status = NULL` rows to `'pending'`,
  but rescanning unchanged files returns `UpsertOutcome::Unchanged`
  and therefore never re-enqueues them — those rows stay `pending`
  forever. Tracked as
  [utof/perima#19](https://github.com/utof/perima/issues/19);
  mitigation (retry command OR enqueue-on-Unchanged-AND-pending) will
  land in a later patch.

[0.4.3]: https://github.com/utof/perima/releases/tag/v0.4.3

## [0.4.2] — 2026-04-16

Phase 4 hotfix pass. Resolves the 1 CRIT + 4 HIGH findings surfaced by
the `codex:rescue` adversarial review of v0.4.1 (utof/perima#15). No
new user-facing features; all changes are correctness / security
fixes. Closes utof/perima#15.

### Security

- **Tauri asset-protocol scope narrowed** (CRIT #15). Dropped the
  `**` wildcard fallback from `assetProtocol.scope`; the wildcard
  effectively granted the WebView read access to any file on disk
  via `convertFileSrc`. Scope is now explicitly
  `$APPDATA/perima/thumbnails/**` + `$APPLOCALDATA/perima/thumbnails/**`
  — OS-portable via Tauri's built-in path variables.

### Fixed

- **`upsert_metadata` no longer clobbers thumbnail columns** (HIGH #4).
  Previously the INSERT + UPDATE statements bound `thumbnail_path` +
  `thumbnail_status` from the `MediaMetadata` struct, which every
  extractor supplies as `None`. A subsequent `Updated` upsert on an
  already-thumbnailed row therefore cleared the state back to NULL.
  The queue worker's `update_thumbnail` is now the sole writer;
  INSERT seeds a literal `'pending'` default; UPDATE never touches
  these columns. Regression test pins the invariant.
- **Video files no longer routed through the image thumbnailer**
  (HIGH #11b). Previously every `video/*` MIME got
  `thumbnail_status='failed'` because `ThumbnailGenerator` decodes
  via `image::ImageReader` and cannot handle MP4/MOV. Video paths
  now short-circuit to `thumbnail_status='skipped'` (new stable
  status distinct from `failed`); the UI placeholder renders the
  unknown-status glyph. Video frame extraction via ffmpeg is
  tracked as a future enhancement.
- **Desktop scan command wires the metadata queue + thumbnailer**
  (HIGH #11a). Previously the Tauri `scan` command only touched
  `file_repo` + `volume_repo` — users scanning via the UI got
  indexed files but no metadata and no thumbnails. Now mirrors the
  CLI's scan wiring: `MetadataQueue` + `ThumbnailGenerator` rooted
  at `data_dir` are spawned up front; each successful
  `Inserted`/`Updated` upsert enqueues; bounded 30 s drain at exit.
  New integration test pins the end-to-end (2 PNG files → 2
  `file_metadata` rows → 2 WebP thumbnails on disk).
- **V004 backfills NULL `thumbnail_status` to `'pending'`** (HIGH
  #3). V003 added the column as nullable without a default, and no
  writer produced `'pending'` — rows from v0.4.0 (pre-thumbnails)
  and `--no-thumbnails` scans stuck at NULL forever, invisible to
  `idx_file_metadata_thumbnail_pending`. V004 one-shot backfills
  existing rows; `upsert_metadata`'s INSERT now seeds `'pending'`
  as a literal default (UPDATE path still untouched per the task-2
  decoupling).

### Notes

- Runtime verification of the Tauri scope change is deferred to user
  testing — no display available in the dev / CI machine.
- Migration V004 is additive and SQL-only. Existing v0.4.0 / v0.4.1
  databases will apply it on first v0.4.2 launch.

[0.4.2]: https://github.com/utof/perima/releases/tag/v0.4.2

## [0.4.1] — 2026-04-16

### Added

- **WebP thumbnails + grid view UI** (phase 4 user-visible tier).
  - `perima-media::ThumbnailGenerator` writes 256px WebP thumbnails
    (Lanczos3 resize, aspect-preserving) to
    `<data_dir>/thumbnails/<aa>/<hash>.webp`. Atomic write via
    `.tmp` + `fs::rename` — mid-write crashes cannot leave a
    half-written file.
  - V003 migration adds `thumbnail_path` + `thumbnail_status` to
    `file_metadata` (nullable, additive; existing v0.4.0 rows
    read as "not yet processed"). Partial index on
    `thumbnail_status = 'pending' AND deleted_at IS NULL` supports
    a future `perima thumbnail` retry command.
  - `MetadataRepository::update_thumbnail` — separate trait method
    so the queue worker's thumbnail result always persists without
    colliding with `upsert_metadata`'s Unchanged equivalence proxy.
    Wrapped in `BEGIN IMMEDIATE`.
  - `MetadataQueue` worker now calls the thumbnailer after metadata
    extraction for image/video MIMEs; on success writes
    `thumbnail_status = 'ready'` + absolute path; on failure writes
    `'failed'` and continues (no worker abort).
  - `ThumbnailGenerator::disabled()` constructor + new
    `perima scan --no-thumbnails` flag shortcircuit the thumbnail
    write path for users wanting faster scans.
- **Desktop grid view.** New `FileGrid` component renders 200px
  tiles with thumbnails via Tauri's asset protocol
  (`convertFileSrc`). Header gains a Table / Grid toggle; default is
  Table for v0.3.x UX continuity. Placeholder icons for pending /
  failed / unknown tiles. 3 new vitest tests (14 total).
- `tauri.conf.json` enables `assetProtocol` with scope
  `$APPDATA/perima/thumbnails/**` + broad `**` fallback.

### Changed

- Desktop app now calls `list_files_with_metadata` (added in v0.4.0)
  for both Table and Grid views. `FileWithMetadataPayload` gains
  `thumbnail_path` + `thumbnail_status` fields.
- Tauri dep gains the `protocol-asset` feature flag.

[0.4.1]: https://github.com/utof/perima/releases/tag/v0.4.1

## [0.4.0] — 2026-04-16

First MINOR release under release-plz infrastructure (utof/perima#10).
**Note:** release-plz did not auto-open the release PR despite 5 feat
commits on main. Falling back to manual tag for this release; config
tuning filed as follow-up (see Project section below).

### Added

- **Media metadata extraction + background queue** (utof/perima phase 4).
  - `perima-core`: `MediaMetadata` value type + `MetadataExtractor` trait
    (MIME-dispatched, not first-non-empty) + `MetadataRepository` port
    using `&self` with interior mutability.
  - `perima-db`: V002 migration adds `file_metadata` table (content-
    addressed by `blake3_hash` PK, matching `files`; CRDT-compliant;
    partial index on `captured_at WHERE deleted_at IS NULL`).
  - `perima-db::SqliteMetadataRepository`: `Mutex<Connection>` +
    `BEGIN IMMEDIATE` upsert mirroring v0.3.1 hardened pattern;
    `list_with_metadata` uses LEFT JOIN with `fm.updated_at` as the
    NULL sentinel.
  - **New crate `perima-media`**: `ImageExtractor` (image + kamadak-exif)
    for JPEG/PNG/WebP/GIF; `VideoExtractor` (mp4parse) for MP4/MOV;
    `CompositeExtractor` dispatches by MIME; `MetadataQueue` with
    sync `enqueue` via `try_send` + 50ms poll loop watching
    `CancellationToken`.
  - CLI: `perima metadata <path>` subcommand extracts metadata for a
    single file. `perima ls --with-metadata` adds captured_at +
    dimensions + camera_model columns.
  - Scan integration: enqueues freshly hashed files after
    `Inserted`/`Updated`. Bounded 30s drain on scan exit; new
    `--no-wait-metadata` flag bypasses drain.
  - Desktop: new `list_files_with_metadata` Tauri command with
    `FileWithMetadataPayload` (specta-typed). `AppState` now holds
    `Arc<SqliteMetadataRepository>`. TS types + API wrapper added
    (grid view lands in v0.4.1).

- **Runtime-generated test fixtures** for `perima-media`: minimal
  JPEG/MP4 assembled in-test via `image` + `mp4` + kamadak-exif's
  experimental writer. No binary blobs committed to git.

### Changed

- CLI `scan::run` is now `async` and carries a `MetadataQueue`. Scan
  waits up to 30s for the queue worker to drain after walking
  completes, unless `--no-wait-metadata` is passed.
- `scripts/pre-commit` now exports Tauri build env vars
  (PKG_CONFIG_PATH, LIBRARY_PATH, RUSTFLAGS) so `just ci` succeeds on
  this dev machine without shell-specific wrappers.

### Project

- release-plz wired (chore-only runs verified) but did not auto-open
  PRs for this release — the per-crate release-aggregation config
  needs more tuning (all feats landed in dep crates; only `perima`
  was `release = true`; dep graph not followed). Filed as
  follow-up: reconfigure release-plz to aggregate feat commits from
  `perima-core`/`-db`/`-fs`/`-media` into `perima`'s release PR for
  v0.4.1+.
- First manually tagged release since release-plz was wired. Future
  minor/patch releases will automate from this point.

## [0.3.2] — 2026-04-16

### Fixed

- **Watcher errors surface in the desktop UI** (utof/perima#8).
  `App.tsx` wires `subscribeToFileEvents` + `startWatch` failure
  paths into a dismissible `WatcherBanner`. Previously these
  errors only logged to `console.warn`, leaving users to wonder
  why the file table had stopped refreshing.
- **`perima_desktop::run` no longer panics on config errors.**
  Replaced `expect()` with `?` propagation into a new
  `RunError = Box<dyn Error + Send + Sync>` alias that matches
  the error type Tauri's `.setup()` callback already expects.
  (Minor public API change; no in-tree callers.)

### Added

- `WatcherBanner` component (`role="alert"`, yellow non-blocking
  treatment distinct from scan errors).
- Unit test for `WatcherState` cancel-token lifecycle.
- Vitest test for watcher subscribe failure → banner renders.

## [0.3.1] — 2026-04-16

### Fixed

- **DB identity invariants** (utof/perima#7, #9).
  - `record_mount` soft-deletes superseded rows for the same
    `(volume_id, machine_id)` before inserting a new mount path;
    stops stale mounts accumulating in
    `VolumeRecord.mounts_on_this_machine`.
  - `update_location_path` checks for an existing active row at the
    destination before renaming. On collision the source row is
    soft-deleted (destination wins, LWW).
  - `upsert_location`, `find_or_create`, `update_location_path`, and
    `record_mount` wrap their SELECT-then-INSERT/UPDATE sequences in
    `BEGIN IMMEDIATE`; concurrent CLI + desktop writers can no
    longer produce duplicate active rows.
  - `conn.busy_timeout(5s)` at connection open so contending
    `BEGIN IMMEDIATE` requests serialize instead of erroring.
  - Non-UTF-8 mount paths now return `CoreError::InvalidPath`
    instead of silent lossy conversion via `to_string_lossy`.

### Added

- Deterministic concurrent-race tests via `std::sync::Barrier`
  covering `find_or_create` and `upsert_location` across two
  independent SQLite connections.
- Connection-level test for `busy_timeout` serialization using
  channel-synchronized two-thread ordering.

## [0.3.0] — 2026-04-16

### Added

- **Filesystem watching.** A new `perima watch <path>` CLI subcommand watches a
  folder recursively and updates the database in real time as files change.
  Built on `notify-debouncer-full` with a 1-second debounce.
- **`FileEvent` + `EventBus` trait** in `perima-core` — `Created`, `Modified`,
  `Deleted`, `Renamed` variants, framework-free. `CompositeEventBus` fans out
  to multiple handlers.
- **`DbEventHandler`** maps filesystem events to database mutations:
  `Modified → Stale`, `Deleted → Missing`, `Renamed → update_path`.
- **`Stale` variant** of `LocationStatus` — indicates a file's stored BLAKE3
  hash is outdated (rehashing deferred to the next scan for responsiveness).
- **Tauri watcher commands** — `start_watch`, `stop_watch`, `is_watching`.
  `TauriEventEmitter` broadcasts `file-event` events to the frontend.
- **Frontend live refresh.** The React desktop app subscribes to `file-event`
  and debounces table refreshes at 300 ms. Auto-starts the watcher after a
  successful scan.
- `perima ls` now renders `stale` status alongside `active`/`missing`/`moved`.

### Changed

- **Cancellation migrated from `AtomicBool` to
  `tokio_util::sync::CancellationToken`** across CLI and desktop. Unifies
  the shutdown path between scan + watch.
- **CLI `main()` is now async** (`#[tokio::main]`) to host the watcher's
  background task. Existing synchronous commands (`scan`, `ls`, `volumes`)
  continue to run on the main task without yielding.
- **`tokio` + `tokio-util`** are now runtime dependencies (previously
  test-only).
- Workspace version is now centralized in `[workspace.package]`; all crates
  inherit via `version.workspace = true`.

### Fixed

- Watcher paths are now canonicalized via `dunce::canonicalize`, fixing
  silently-dropped events on macOS where `tempdir()` returns
  `/var/folders/...` (a symlink) but FSEvents reports the canonical
  `/private/var/folders/...` form.
- macOS-specific watcher quirks are documented; delete/rename tests are
  gated to Linux where `notify` semantics are stable.
  See [#5](https://github.com/utof/perima/issues/5).

### Project

- First release under the new **semver + conventional-commits** convention.
  Prior milestones (`phase-0-complete` through `phase-2-complete`) remain as
  historical tags. Going forward, releases are `v0.N.x` tags with CHANGELOG
  entries generated from conventional commit messages.
- Commit scopes now name codebase components (`core`, `db`, `fs`, `hash`,
  `cli`, `desktop`, `ci`, `deps`, `docs`, `release`) rather than development
  milestones.

[Unreleased]: https://github.com/utof/perima/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/utof/perima/releases/tag/v0.5.1
[0.4.0]: https://github.com/utof/perima/releases/tag/v0.4.0
[0.3.2]: https://github.com/utof/perima/releases/tag/v0.3.2
[0.3.1]: https://github.com/utof/perima/releases/tag/v0.3.1
[0.3.0]: https://github.com/utof/perima/releases/tag/v0.3.0
