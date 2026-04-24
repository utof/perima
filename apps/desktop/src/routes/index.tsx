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
import TagSidebar from "../components/TagSidebar";
import { composeVisible, computeFacets, sortByRank } from "../lib/search";

export default function IndexRoute() {
  const { data: files = [], isLoading: filesLoading } = useFiles(100);
  const { data: tags = [] } = useTags();
  const viewMode = useUiStore((s) => s.viewMode);
  const selectedTagId = useUiStore((s) => s.selectedTagId); // WHY kept: still used by composeVisible below
  // WHY debouncedQuery (not searchQuery): per spec §5.11 dual-field store,
  // only the post-300ms-debounce sanitised query drives `useSearch`. The
  // raw `searchQuery` field exists for the input value binding only.
  const debouncedQuery = useUiStore((s) => s.debouncedQuery);
  const { data: searchHits } = useSearch(debouncedQuery);

  // WHY undefined-check: useSearch returns `data: SearchHit[] | undefined` —
  // undefined when query.length < MIN_QUERY_LEN per the `enabled` clause.
  // Undefined means "no search active" → searchActive = false.
  const searchActive = searchHits !== undefined;
  const hashSet = searchActive ? new Set(searchHits.map((h) => h.blake3_hash)) : null;
  const rankMap = searchActive
    ? new Map(searchHits.map((h) => [h.blake3_hash, h.rank]))
    : new Map<string, number>();
  const baseVisible = composeVisible(files, selectedTagId, hashSet);
  const visibleFiles = searchActive ? sortByRank(baseVisible, rankMap) : baseVisible;
  const facetCounts = computeFacets(visibleFiles);

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
    </div>
  );
}
