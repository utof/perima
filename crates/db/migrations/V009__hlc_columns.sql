-- V009: HLC (Hybrid Logical Clock) columns on CRDT-eligible rows.
--
-- WHY: per architecture audit §4.8, lock in the ordering primitive
-- needed for any post-v1 CRDT integration (Loro, Automerge, Yrs)
-- BEFORE the integration happens. Additive, nullable — populated
-- lazily by Batch B+ writer paths. No change to existing `updated_at`
-- semantics.
--
-- Exclusions: `volume_mounts`, `scan_progress`, `thumbnail_queue`,
-- `device_config` are intrinsically device-local (machine_id scoped)
-- and never synced — no hlc column.
--
-- Packing: 48 low-bits ms + 16 high-bits counter → non-negative i64.
-- Ord over i64 matches Ord over (ms, counter). See
-- `crates/core::Hlc` for the helper.

ALTER TABLE files           ADD COLUMN hlc INTEGER;
ALTER TABLE file_locations  ADD COLUMN hlc INTEGER;
ALTER TABLE file_metadata   ADD COLUMN hlc INTEGER;
ALTER TABLE tags            ADD COLUMN hlc INTEGER;
ALTER TABLE file_tags       ADD COLUMN hlc INTEGER;
ALTER TABLE volumes         ADD COLUMN hlc INTEGER;
