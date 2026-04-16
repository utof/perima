-- WHY: blake3_hash is the PK on files because a BLAKE3-256 hash is
-- deterministic and content-derived — two devices hashing identical
-- bytes MUST compute the same value, making it CRDT-merge-safe
-- (effectively a deterministic UUID). The UUIDv7 rule applies only
-- to rows whose identity is NOT content-derived.
CREATE TABLE files (
    blake3_hash   TEXT PRIMARY KEY,
    file_size     INTEGER NOT NULL,
    first_seen    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    deleted_at    TEXT,
    device_id     TEXT NOT NULL
);

CREATE TABLE file_locations (
    id             TEXT PRIMARY KEY,
    blake3_hash    TEXT NOT NULL,
    volume_id      TEXT NOT NULL,
    relative_path  TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'active',
    last_verified  TEXT,
    first_seen     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    deleted_at     TEXT,
    device_id      TEXT NOT NULL
);

CREATE INDEX idx_file_locations_blake3
    ON file_locations(blake3_hash);
CREATE INDEX idx_file_locations_volume_path
    ON file_locations(volume_id, relative_path);

CREATE TABLE volumes (
    volume_id          TEXT PRIMARY KEY,
    gpt_partition_guid TEXT,
    fs_uuid            TEXT,
    volume_label       TEXT,
    capacity_bytes     INTEGER NOT NULL,
    is_removable       INTEGER NOT NULL,
    last_seen          TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    deleted_at         TEXT,
    device_id          TEXT NOT NULL
);

CREATE TABLE volume_mounts (
    id          TEXT PRIMARY KEY,
    volume_id   TEXT NOT NULL,
    machine_id  TEXT NOT NULL,
    mount_path  TEXT NOT NULL,
    first_seen  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    deleted_at  TEXT,
    device_id   TEXT NOT NULL
);

CREATE INDEX idx_volume_mounts_volume_machine
    ON volume_mounts(volume_id, machine_id);
