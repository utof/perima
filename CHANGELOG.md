# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While on `0.x`, per the Cargo / Rust ecosystem convention, **MINOR version bumps
may include breaking API changes**; PATCH releases are additive/fix-only. The
project will remain on `0.x` until an API stability commitment is made — no fixed
roadmap milestone triggers `1.0.0`.

## [Unreleased]

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

[Unreleased]: https://github.com/utof/perima/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/utof/perima/releases/tag/v0.3.1
[0.3.0]: https://github.com/utof/perima/releases/tag/v0.3.0
