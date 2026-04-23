/**
 * Singleton TanStack Query client + custom-error type augmentation.
 *
 * WHY Register augmentation (not per-call generics): per the v5 TS docs,
 * explicit `useQuery<T, CoreError>` generics break `T` inference from
 * `queryOptions`. Module augmentation makes `error: CoreError | null`
 * the default everywhere with zero per-call generics. Verified
 * 2026-04-23 against TanStack Query docs.
 */
import { QueryClient } from "@tanstack/react-query";
import type { CoreError } from "../bindings";

declare module "@tanstack/react-query" {
  interface Register {
    defaultError: CoreError;
  }
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // WHY 5min staleTime: Tauri runs locally; AppEvent-driven
      // invalidation handles real changes. Refetching on every mount
      // would hammer the writer thread for no UX win.
      staleTime: 5 * 60 * 1000,
      gcTime: 30 * 60 * 1000,
      // WHY false: a desktop window losing focus is not a "data may be
      // stale" signal. We're not a web app multiplexing tabs.
      refetchOnWindowFocus: false,
      // WHY false: no network — IPC is always available.
      refetchOnReconnect: false,
      // WHY false: Tauri command failures are deterministic; retry
      // won't help; surface CoreError to the user immediately.
      retry: false,
    },
    mutations: {
      retry: false,
    },
  },
});
