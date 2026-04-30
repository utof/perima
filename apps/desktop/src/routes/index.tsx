/**
 * Index route — file/tag/search composition.
 *
 * State sources:
 *   - server: useFiles, useTags, useSearch (TanStack Query)
 *   - UI:     useUiStore (viewMode, selectedTagId, debouncedQuery)
 *   - derived: composeVisible / sortByRank / computeFacets (pure fns)
 *
 * WHY no manual useMemo on derivations: React Compiler 1.0 (L2) handles
 * referentially-stable inputs automatically. Adding useMemo here is a
 * regression per the L2 standing constraint.
 */
import { useUiStore } from "../stores/ui";
import { useFiles } from "../queries/files";
import { useTags } from "../queries/tags";
import { useSearch } from "../queries/search";
import FileGrid from "../components/FileGrid";
import FileTable from "../components/FileTable";
import FileSidebar from "../components/FileSidebar";
import TagSidebar from "../components/TagSidebar";
import { composeVisible, computeFacets, sortByRank } from "../lib/search";

export default function IndexRoute() {
  // WHY 1000 (was 100): the v0.6.x display path uses an intersection
  // pattern — visible files = useFiles(N) ∩ search hits. With N=100 a
  // library larger than 100 files showed only the search hits that
  // happened to be in the first-100 page (e.g. searching "mp4" in a
  // 340-file library returned 3 of 339 actual matches). Bumping to
  // 1000 covers the common case (<1k files) without rearchitecting.
  // Real fix for 10k+ libraries: virtualisation + result-driven
  // pagination; tracked separately.
  const { data: files = [], isLoading: filesLoading } = useFiles(1000);
  const { data: tags = [] } = useTags();
  const viewMode = useUiStore((s) => s.viewMode);
  const selectedTagId = useUiStore((s) => s.selectedTagId); // WHY kept: still used by composeVisible below
  // WHY debouncedQuery (not searchQuery): per spec §5.11 dual-field store,
  // only the post-300ms-debounce sanitised query drives `useSearch`. The
  // raw `searchQuery` field exists for the input value binding only.
  const debouncedQuery = useUiStore((s) => s.debouncedQuery);
  const selectedFileUuid = useUiStore((s) => s.selectedFileUuid);
  const setSelectedFileUuid = useUiStore((s) => s.setSelectedFileUuid);
  const { data: searchHits } = useSearch(debouncedQuery);

  // WHY undefined-check: useSearch returns `data: SearchHit[] | undefined` —
  // undefined when query.length < MIN_QUERY_LEN per the `enabled` clause.
  // Undefined means "no search active" → searchActive = false.
  const searchActive = searchHits !== undefined;
  // WHY filter+typeguard (Task 11, spec §4.8): `SearchHit.blake3_hash` is now
  // `string | null` because pending files (no `full_hash` yet) can still hit
  // the FTS index. The hash-keyed compose/sort helpers operate on `Set<string>`
  // / `Map<string, number>`, so we drop nullable hashes before constructing
  // them. Pending files surface in the underlying `files` list and pass
  // through `composeVisible` only when no search filter is active.
  const hashSet = searchActive
    ? new Set(
        searchHits
          .map((h) => h.blake3_hash)
          .filter((h): h is string => h !== null),
      )
    : null;
  const rankMap = searchActive
    ? new Map(
        searchHits
          .filter((h): h is typeof h & { blake3_hash: string } => h.blake3_hash !== null)
          .map((h) => [h.blake3_hash, h.rank]),
      )
    : new Map<string, number>();
  const baseVisible = composeVisible(files, selectedTagId, hashSet);
  const visibleFiles = searchActive ? sortByRank(baseVisible, rankMap) : baseVisible;
  const facetCounts = computeFacets(visibleFiles);

  // Derive the selected file object from the UUID so FileSidebar gets a
  // fully-typed FileWithTagsPayload without a second IPC round-trip.
  // WHY find over a separate query: visibleFiles is already in memory;
  // avoid a second IPC call for a single-row lookup at this scale.
  const selectedFile = selectedFileUuid !== null
    ? visibleFiles.find((f) => f.file_uuid === selectedFileUuid) ?? null
    : null;

  return (
    <div className="flex-1 flex overflow-hidden">
      {tags.length > 0 && (
        <TagSidebar
          tags={tags}
          counts={facetCounts}
          totalCount={searchActive ? visibleFiles.length : files.length}
          mode={searchActive ? "facets" : "all"}
        />
      )}
      <main className="flex-1 overflow-auto p-4">
        {viewMode === "table"
          ? <FileTable files={visibleFiles} loading={filesLoading} />
          : <FileGrid files={visibleFiles} loading={filesLoading} />}
      </main>
      {selectedFile !== null && (
        <FileSidebar
          file={selectedFile}
          onClose={() => { setSelectedFileUuid(null); }}
        />
      )}
    </div>
  );
}
