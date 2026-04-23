/**
 * useDomainEvents — hook-level test for AppEvent → invalidateQueries dispatch.
 *
 * WHY hook-only (not full <App />): Batch H moved invalidation logic into
 * the hook; the test mounts only the hook inside a QueryClientProvider via
 * `renderHook`, captures the AppEvent handler passed into
 * `subscribeToAppEvents`, fires synthetic events, and asserts on the
 * `queryClient.invalidateQueries` spy. We do NOT count `invoke` calls —
 * the prior App-level approach was confounded by auto-mount fetches.
 */
import { renderHook, waitFor, act } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach, afterEach } from "vitest";
import type { ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import * as api from "../../api";
import type { AppEvent, UnsubscribeFn } from "../../api";
import { useDomainEvents } from "../../hooks/useDomainEvents";
import { filesKeys } from "../../queries/files";
import { tagsKeys } from "../../queries/tags";
import { searchKeys } from "../../queries/search";
import { useUiStore } from "../../stores/ui";
import { makeFreshQueryClient, resetUiStore } from "../test-utils";

// WHY mock the api module: real subscribeToAppEvents calls Tauri's `listen`
// which is also mocked in setup.ts; mocking at the api boundary is cleaner —
// we capture the AppEvent callback directly without unwrapping `payload`.
vi.mock("../../api", async () => {
  const actual = await vi.importActual<typeof import("../../api")>("../../api");
  return {
    ...actual,
    subscribeToAppEvents: vi.fn(),
  };
});

const mockSubscribe = vi.mocked(api.subscribeToAppEvents);

function createSubscription() {
  let captured: ((event: AppEvent) => void) | null = null;
  const unsubscribe: UnsubscribeFn = vi.fn();
  mockSubscribe.mockImplementation((callback) => {
    captured = callback;
    return Promise.resolve(unsubscribe);
  });
  return {
    fire: (event: AppEvent) => {
      if (!captured) {
        throw new Error(
          "AppEvent handler was never captured (subscribe not yet awaited)",
        );
      }
      captured(event);
    },
    unsubscribe,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useDomainEvents", () => {
  test("File event debounces a 300ms files invalidation", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });

    // Wait for the subscribe promise to resolve and the handler to be captured.
    // WHY runAllTimersAsync: subscribe is an async chain inside a useEffect;
    // microtasks flushed via the timer pump.
    await vi.runAllTimersAsync();
    expect(mockSubscribe).toHaveBeenCalledTimes(1);

    invalidateSpy.mockClear();

    // Fire 5 rapid File events — the hook's debounce should coalesce.
    act(() => {
      for (let i = 0; i < 5; i++) {
        sub.fire({
          kind: "File",
          data: {
            type: "Created",
            path: `file${i}.txt`,
            volume: "00000000-0000-0000-0000-000000000000",
          },
        });
      }
    });

    // Before the 300ms timer fires, no invalidation has happened.
    expect(invalidateSpy).not.toHaveBeenCalled();

    // Advance past the debounce window.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    const filesCalls = invalidateSpy.mock.calls.filter(
      ([arg]) =>
        Array.isArray(arg?.queryKey) && arg.queryKey[0] === filesKeys.all[0],
    );
    expect(filesCalls).toHaveLength(1);
  });

  test("ScanCompleted invalidates files + tags immediately (no debounce)", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();
    invalidateSpy.mockClear();

    act(() => {
      sub.fire({
        kind: "ScanCompleted",
        data: {
          volume: "00000000-0000-0000-0000-000000000000",
          files_seen: 10,
          files_new: 3,
          duration_ms: 1234,
        },
      });
    });

    // No timer advance — these fire immediately.
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: filesKeys.all });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: tagsKeys.all });
  });

  test("IndexInvalidated.TagsChanged invalidates only tags", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();
    invalidateSpy.mockClear();

    act(() => {
      sub.fire({ kind: "IndexInvalidated", data: { reason: "TagsChanged" } });
    });

    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: tagsKeys.all });
    expect(invalidateSpy).not.toHaveBeenCalledWith({ queryKey: filesKeys.all });
    expect(invalidateSpy).not.toHaveBeenCalledWith({ queryKey: searchKeys.all });
  });

  test("IndexInvalidated.SearchIndexRebuilt invalidates only search", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();
    invalidateSpy.mockClear();

    act(() => {
      sub.fire({ kind: "IndexInvalidated", data: { reason: "SearchIndexRebuilt" } });
    });

    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: searchKeys.all });
    expect(invalidateSpy).not.toHaveBeenCalledWith({ queryKey: tagsKeys.all });
  });

  test("IndexInvalidated.FilesChanged debounces a files invalidation", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();
    invalidateSpy.mockClear();

    act(() => {
      sub.fire({ kind: "IndexInvalidated", data: { reason: "FilesChanged" } });
    });

    expect(invalidateSpy).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: filesKeys.all });
  });

  test("subscribe failure surfaces a notifyError into the store", async () => {
    mockSubscribe.mockRejectedValueOnce(new Error("channel closed"));
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });

    await waitFor(() => {
      const notes = useUiStore.getState().notifications;
      expect(notes).toHaveLength(1);
      expect(notes[0]?.message).toMatch(
        /Failed to subscribe to app events.*channel closed/,
      );
    });
  });
});
