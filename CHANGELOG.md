# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While on `0.x`, per the Cargo / Rust ecosystem convention, **MINOR version bumps
may include breaking API changes**; PATCH releases are additive/fix-only. The
project will remain on `0.x` until an API stability commitment is made — no fixed
roadmap milestone triggers `1.0.0`.

## [Unreleased]

## [0.6.4] — 2026-04-18

### Fixed

- **Deleted-tag token leak in FTS.** V007 aggregations filtered `ft.deleted_at IS NULL` but never `t.deleted_at IS NULL`; soft-deleting a tag left its name tokens matchable in search forever, even after `rebuild()`. V008 adds the missing filter to every aggregation site + a dedicated `search_after_tag_soft_delete_or_restore` trigger covering both transition directions.
- **Representative-path overwrite on hash change.** V007's `search_after_location_hash_change` rewrote `search_content.filename`/`relative_path` with `NEW.*` values on every hash change, clobbering the first-seen representative's indexed path when a non-rep location's hash flipped to a hash that already had a representative. V008 splits the trigger into `_retire` (OLD hash; always) and `_seed` (NEW hash; only when `NEW.deleted_at IS NULL`); `_seed`'s refresh reads path from joined live state via the same first-seen-active subquery used everywhere else.
- **Combined hash-change + soft-delete leaked tombstoned NEW hash.** The split `_seed` trigger now WHEN-gates on `NEW.deleted_at IS NULL`; a single UPDATE mutating both `blake3_hash` and `deleted_at` no longer inserts a live FTS doc for a tombstoned row.
- **Location restore was a no-op.** V007 had no inverse for `search_after_location_soft_delete`; clearing `deleted_at` on a previously-retired row never recreated the FTS doc. New `search_after_location_restore` trigger refreshes from joined live state.
- **Metadata soft-delete left tokens indexed.** V007's `search_after_metadata_update` blindly copied `NEW.mime_type` / `camera_model` / `captured_at` with no `deleted_at` guard. V008's body uses CASE on `NEW.deleted_at` — live metadata copies values, tombstoned metadata clears them. Single body handles both transition directions.
- **Tombstoned-metadata INSERT seeded live tokens (CRDT-merge scenario).** Post-V008 reviewer caught that `search_after_metadata_insert` was still unguarded — a CRDT merge replicating a soft-deleted metadata row as an INSERT would seed tokens from a row the peer had already deleted. Trigger now WHEN-gates on `NEW.deleted_at IS NULL`.
- **Fresh-location seed re-resurrected tombstoned tags / metadata.** Same reviewer pass: `search_after_file_locations_insert` joined `file_tags`/`file_metadata` without `t.deleted_at IS NULL` / `m.deleted_at IS NULL` filters. A new location inserted after prior-location retirement re-seeded `search_content` with tokens from soft-deleted tags and metadata. V008 adds the missing filters.
- **Volume-filter SQL defeated its own index.** `list_file_locations` and `list_with_metadata` used `(?1 IS NULL OR fl.volume_id = ?1)`; EXPLAIN QUERY PLAN confirmed the OR-with-NULL predicate bypassed `idx_file_locations_volume_path` even when a concrete `volume_id` was supplied. Both now branch at the Rust layer into index-eligible SQL per `vol_filter.is_some()`.
- **`rebuild()` dropped the same `t.deleted_at` / `m.deleted_at` filters.** Aligned with V008's trigger aggregations so incremental and bulk-rebuild paths converge on identical ground truth.

### Added

- **V008 migration: `search_triggers_soft_delete.sql`.** Drops + recreates 8 V007 triggers with correct filters, adds 3 new triggers (`search_after_tag_soft_delete_or_restore`, `search_after_location_restore`, split `_retire`/`_seed` for hash change), + partial covering index `idx_file_locations_rep_active(blake3_hash, first_seen, id) WHERE deleted_at IS NULL` for representative-selection subqueries.
- **Ground-truth proptest** (`fts_matches_ground_truth_under_soft_delete_churn`). 9-op soft-delete universe (attach/detach tag, soft-delete/restore tag, set/soft-delete/restore metadata, soft-delete/restore location) × 256 cases. Invariant: `search_content` computed via per-field ground-truth subqueries — **independent of `rebuild()`'s SQL shape**, so a future bug re-entering both trigger and rebuild paths still fails here.
- **Regression tests T43–T48** pinning every fixed bug. Red-before-fix, green-after — verified during systematic debugging phase.

### Process

- Three-LLM adversarial chain: codex (GPT-5.4) adversarial audit → Opus reviewer on V008 draft → Opus delta-reviewer on round-2 fixes. Chain caught 11/11 in-class findings; delta review returned clean. Methodology captured in `docs/verification-plan.md` for the remaining 5 scopes (fs, hash, desktop, frontend, tests, architecture).
- Follow-up issues filed, not blockers: #56 (proptest op-universe — add RehashLocation + RenameLocation), #57 (EXPLAIN QUERY PLAN assertion test for partial-index adoption), #58 (SQL migration-file linter), #59 (view-based aggregation refactor — v0.7 candidate), #60 (rebuild() startup scalability), #61 (error taxonomy granularity).

## [0.6.3] — 2026-04-17

### Fixed

- **#40 FTS5 contentless-delete blank payloads.** V006's sync triggers tried to remove old tokens with empty-string payloads on a contentless FTS5 table — a silent no-op. Stale tokens accumulated on metadata updates and tag detaches.
- **#41 tag attach on metadata-less file.** `search_rowid_map` only seeded on `file_metadata` insert, so tag attach on a file without metadata was silently ignored until rebuild.
- **#22 FTS5 stale-rename.** No `file_locations` UPDATE trigger — rename (same hash, new path) left old path indexed.
- **#42 hash change at same path.** No trigger on `file_locations.blake3_hash` change — replace-in-place left stale FTS doc.

### Changed

- **V007 migration: external-content FTS5.** Switch from contentless (`content=""`) to external-content (`content='search_content'`). External-content supports `INSERT INTO search_index(search_index, rowid) VALUES('delete', rowid)` — delete-by-rowid without needing OLD column values. `search_rowid_map` dropped; the new `search_content` materialised table takes its role.
- **Ten triggers in six logical categories** cover every indexable mutation surface. Trigger 2 is split 2a/2b/2c by WHEN-gate (hash change, rename, soft-delete). Universal invariant: every trigger body reinserts `search_content` from joined live state (never `NEW.*` for column values), so fire-order is irrelevant across same-transaction multi-table updates.
- **Representative-selection rule:** first-seen active `file_locations` per hash, tiebroken by `id ASC`. Consistent across bulk-populate, rebuild, trigger 2b guard, trigger 2c re-point, and `search()` join.
- **SearchRepository::rebuild()** rewritten to match new schema.
- **Build env cleanup** (`646d6c9`): `.cargo/config.toml` + `justfile` exports eliminate the per-command `export RUSTFLAGS=... export PKG_CONFIG_PATH=...` ritual. `just ci` now works from a plain shell.
- **release-plz-pr job dropped** from `.github/workflows/release-plz.yml` (`2d63597`). We use direct `chore(release):` commits, not PR-gated releases. Revisit at v1.0 ceremony.

### Notes

- **Migration on launch** re-indexes your library. Transparent to end-users but may add a one-time delay on first launch after upgrade for large libraries (>100k files). Progress logging tracked as post-v1 polish.
- **Multi-location semantics unchanged:** one FTS doc per hash, indexed under first-seen active location (v0.6.0 rule). Multi-path indexing remains out of scope. Non-representative location renames don't affect search.
- **32 new tests** landed (4 regression + 6 integration + 1 proptest w/ 256 cases by default).

## [0.6.2] — 2026-04-17

### Fixed

- **Search + tag filter composition (#25).** In v0.6.1, clicking a
  search result silently cleared the tag-sidebar selection, making
  tag-scoped search impossible. v0.6.2 makes the two filters AND-compose:
  the visible list is always the intersection of the active tag (if
  any) and the active search match set (if any). Pinned by
  `App.compose.test.tsx` snapshot.
- **SearchBar feedback loop.** The original Task 4+5+6 bundle had
  `onQueryChange` in the SearchBar `useEffect` deps and App.tsx passed
  a fresh closure every render — causing `api.search` to re-fire on
  every parent re-render. Fixed in `c1d5c17`: `handleSearchChange`
  wrapped in `useCallback` with empty deps; regression test added.

### Changed

- **Search UX: live inline narrowing + facet sidebar** (#32).
  - Typing in the search bar now narrows the file list in place; the
    dropdown preview is removed.
  - While a search is active the list re-sorts by FTS5 BM25 rank
    (lower = better match); the prior sort restores when the search
    clears.
  - The tag sidebar transforms into a facet panel when a search is
    active: only tags present in the current visible result set are
    shown, with live counts that reflect the narrowed list.
  - Clicking a sidebar tag AND-composes with the active search
    (instead of replacing it). Clicking again toggles the tag filter
    off.
- **SearchBar input limit raised from 50 → 500 results** per query to
  feed the in-list re-sort (Tauri command clamps at 500 server-side).
- **Query sanitiser** added: plain-text input is auto-quoted; explicit
  phrase queries (`"blue ridge"`) and prefix queries (`sunse*`) are
  honoured; unsafe chars (bare parens, leading dashes, unpaired
  quotes) are stripped. Advanced FTS5 operators (NEAR, column
  filters, AND/OR keywords) are not exposed — tracked for post-v1
  query DSL.

### Notes

- **Facet counts reflect visible result set only** (capped at 100
  rows via `listFilesWithTags(100)`). Full-corpus counts are a
  post-v1 optimization.
- **Escape key** only clears the search input while focused. Full
  Escape stack (pop filters, pop detail view, etc.) is scope of #28
  (keyboard registry) and #27 (three-pane layout).
- **Stale search hits after watcher refresh:** when the file watcher
  fires a list refresh while a search is active, `searchHits` is not
  automatically re-queried. User can retype to re-fire. Post-v1 fix:
  auto-re-search on watcher-driven refresh.
- **Process:** Task 4+5+6 bundled into a single commit `8fa9bf9`
  because the three components share a compile-time type contract
  (SearchBar prop shape, TagSidebar optional `mode` prop, App.tsx
  composition derivation) that cannot be changed in isolation under
  the workspace's no-`--no-verify` rule. Follow-up `fix(desktop):
  c1d5c17` addresses the two Critical/Important findings from the
  bundle's Opus review.

## [0.6.1] — 2026-04-16

Desktop search UI layer on top of the v0.6.0 search backend.

### Added

- **`SearchHitPayload` + Tauri commands** (`perima-desktop`): `search(query,
  limit)` returns ranked `Vec<SearchHitPayload>`; `search_rebuild()` wipes and
  rebuilds the `FTS5` index. Third `WAL`-mode connection in `AppState` avoids
  cross-locking the metadata and tag mutexes.
- **`SearchHit` TS type** + `api.ts` wrappers (`search`, `searchRebuild`) —
  `ResultAsync` pattern consistent with other API functions.
- **`SearchBar` component** — debounced (300 ms) `FTS5` search input with a
  dropdown results panel. Errors are non-fatal (show empty results). Outside
  click closes the panel.
- **`App.tsx` integration** — `SearchBar` in the header; clicking a hit
  filters the file list to that content hash; a `✕ search` badge clears the
  filter. Search takes precedence over the tag sidebar filter.
- Six `SearchBar` unit tests covering debounce timing, empty results, error
  path (non-fatal), hit click callback, and clear button.

## [0.6.0] — 2026-04-16

First full-text search release. Enables `perima search <query>` backed by
an `SQLite FTS5` index kept in sync via SQL triggers.

### Added

- **`SearchHit` type + `SearchRepository` trait** (`perima-core`): value
  type carrying `blake3_hash`, `volume_id`, `relative_path`, and BM25
  `rank`; port trait with `search(query, limit)` and `rebuild()`.
- **V006 migration** (`perima-db`): `search_index` FTS5 virtual table
  (contentless, `unicode61` tokenizer) + `search_rowid_map` side table
  (rowid ↔ `blake3_hash` + `volume_id` + `relative_path`). Four AFTER
  INSERT / AFTER UPDATE triggers on `file_metadata` and `file_tags` keep
  the index current without a background job.
- **`SqliteSearchRepository`** (`perima-db`): BM25-ranked `MATCH` query
  via the FTS5 `rank` auxiliary; `rebuild()` does an atomic
  `BEGIN IMMEDIATE` wipe-and-reindex from the live DB state.
- **`perima search <query>`** (`perima-cli`): plain ranked-table output
  (HASH | PATH | RANK) or `--json` (serde array). `--limit` caps results
  (default 50). `--rebuild` refreshes the index and exits. Four
  integration tests cover rebuild, name-token lookup, JSON shape, and
  empty-result output.

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

[Unreleased]: https://github.com/utof/perima/compare/v0.6.3...HEAD
[0.6.3]: https://github.com/utof/perima/releases/tag/v0.6.3
[0.6.2]: https://github.com/utof/perima/releases/tag/v0.6.2
[0.6.1]: https://github.com/utof/perima/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/utof/perima/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/utof/perima/releases/tag/v0.5.1
[0.4.0]: https://github.com/utof/perima/releases/tag/v0.4.0
[0.3.2]: https://github.com/utof/perima/releases/tag/v0.3.2
[0.3.1]: https://github.com/utof/perima/releases/tag/v0.3.1
[0.3.0]: https://github.com/utof/perima/releases/tag/v0.3.0
