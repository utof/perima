import type { FileWithTagsPayload } from "../../bindings";

/**
 * Construct a FileWithTagsPayload fixture for unit tests.
 *
 * WHY shared: the same fixture shape is needed by search.test.ts
 * (computeFacets, composeVisible, sortByRank tests) and later by
 * App.compose.test.tsx (Task 7 composition snapshot). Keep one source.
 *
 * Only the two fields that matter for filter/compose logic (`hash` and
 * `tags[].id`) take real values; everything else is a neutral default.
 */
export function file(hash: string, tagIds: string[]): FileWithTagsPayload {
  // WHY (Task 11): `file_uuid` is the stable surrogate (always present);
  // `hash` is `string | null` post-spec-§4.8. Tests pass a non-null hash by
  // default so the existing assertions still hold; new pending-file tests
  // construct payloads inline to exercise the null branch.
  return {
    file_uuid: `uuid-${hash}`,
    hash,
    quick_hash: null,
    size: 0,
    volume_id: "vol",
    relative_path: `${hash}.jpg`,
    status: "active",
    first_seen: "2026-01-01T00:00:00Z",
    width: null,
    height: null,
    duration_ms: null,
    captured_at: null,
    camera_make: null,
    camera_model: null,
    codec: null,
    bitrate_bps: null,
    mime_type: null,
    thumbnail_path: null,
    thumbnail_status: null,
    tags: tagIds.map((id) => ({
      id,
      name: `tag-${id}`,
      first_seen: "2026-01-01T00:00:00Z",
    })),
  };
}
