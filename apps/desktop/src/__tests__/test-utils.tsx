/**
 * Test rendering helper — wraps UI in QueryClientProvider with a fresh
 * client per test (gcTime: 0 so cross-test cache doesn't bleed) and
 * resets useUiStore to a known state, optionally seeded with overrides.
 *
 * WHY a single helper: every Batch H test needs the QueryClientProvider
 * + a clean store. Centralising the boilerplate keeps tests focused on
 * assertions rather than provider scaffolding.
 *
 * WHY gcTime: 0: TanStack Query v5 keeps query data alive for `gcTime`
 * after the last observer unmounts. With the prod default of 30min,
 * tests would share cache across vitest runs. gcTime: 0 + a fresh
 * QueryClient per render guarantees isolation.
 *
 * WHY resetUiStore on every renderWithProviders call: the Zustand store
 * is a module-level singleton; without explicit reset, mutations from
 * test N leak into test N+1.
 */
import type { ReactElement } from "react";
import { render, type RenderResult } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useUiStore, type UiStore } from "../stores/ui";

/**
 * Default UI-store state matching slice initial values.
 * WHY const (not derived from `useUiStore.getInitialState()`): Zustand
 * v5 stores expose the action functions on every `getState()` call,
 * so we cannot easily round-trip a "pristine" snapshot. Tests reset
 * by partial-merging this set of leaf primitives, leaving the action
 * functions untouched.
 */
export const defaultUiState = {
  viewMode: "table" as const,
  selectedTagId: null,
  searchQuery: "",
  debouncedQuery: "",
  scan: { status: "idle" as const, lastReport: null },
  notifications: [],
  verifyBatch: null,
  // WHY null: SelectionSlice initial state. Added Task 14 (file-detail sidebar).
  selectedFileUuid: null,
};

/** Reset the UI store to {@link defaultUiState}. Call from `beforeEach`. */
export function resetUiStore(): void {
  useUiStore.setState(defaultUiState);
}

/** Build a QueryClient with test-friendly defaults (no gc, no retry). */
export function makeFreshQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { staleTime: 0, gcTime: 0, retry: false },
      mutations: { retry: false },
    },
  });
}

export interface RenderOptions {
  /** Provide an existing client (e.g. to share across multiple renders). */
  queryClient?: QueryClient;
  /** Partial UI-store overrides applied after the reset. */
  initialStoreState?: Partial<UiStore>;
}

/**
 * Render `ui` inside a QueryClientProvider with a fresh QueryClient and
 * a reset UI store.
 *
 * @returns The RTL RenderResult plus the `queryClient` instance so tests
 * can `vi.spyOn(result.queryClient, "invalidateQueries")` etc.
 */
export function renderWithProviders(
  ui: ReactElement,
  opts: RenderOptions = {},
): RenderResult & { queryClient: QueryClient } {
  const queryClient = opts.queryClient ?? makeFreshQueryClient();
  resetUiStore();
  if (opts.initialStoreState) {
    useUiStore.setState(opts.initialStoreState);
  }
  const result = render(
    <QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>,
  );
  return Object.assign(result, { queryClient });
}
