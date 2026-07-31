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
import { dedupKeys } from "../../queries/dedup";
import { transcriptsKeys } from "../../queries/transcripts";
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

  test("VerifyProgress updates the verifyBatch Zustand slice", async () => {
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await act(async () => { await Promise.resolve(); });

    // Initially the slice is null.
    expect(useUiStore.getState().verifyBatch).toBeNull();

    act(() => {
      sub.fire({
        kind: "VerifyProgress",
        data: {
          batch_id: "batch-001",
          files_done: 1,
          files_total: 3,
          latest_outcome: {
            outcome: "Computed",
            data: {
              file_uuid: "uuid-aaa",
              hash: "a".repeat(64),
            },
          },
        },
      });
    });

    // After the event the slice must reflect the progress payload.
    const state = useUiStore.getState().verifyBatch;
    expect(state).not.toBeNull();
    expect(state?.batchId).toBe("batch-001");
    expect(state?.filesDone).toBe(1);
    expect(state?.filesTotal).toBe(3);
    expect(state?.latestOutcome).toMatchObject({ outcome: "Computed" });
  });

  test("VerifyComplete resets verifyBatch slice and invalidates dedup query", async () => {
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await act(async () => { await Promise.resolve(); });

    // First put something into the slice.
    act(() => {
      sub.fire({
        kind: "VerifyProgress",
        data: {
          batch_id: "batch-002",
          files_done: 2,
          files_total: 2,
          latest_outcome: {
            outcome: "Failed",
            data: {
              file_uuid: "uuid-bbb",
              error: { kind: "Internal", data: "disk read error" },
            },
          },
        },
      });
    });

    expect(useUiStore.getState().verifyBatch).not.toBeNull();
    invalidateSpy.mockClear();

    // Now fire VerifyComplete.
    act(() => {
      sub.fire({ kind: "VerifyComplete", data: { batch_id: "batch-002" } });
    });

    // Slice must be cleared.
    expect(useUiStore.getState().verifyBatch).toBeNull();

    // dedup keys must be invalidated.
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: dedupKeys.all });

    // Files / tags / search must NOT be invalidated by VerifyComplete.
    const calledKeys = invalidateSpy.mock.calls.map(([arg]) => arg?.queryKey);
    expect(calledKeys).not.toContainEqual(filesKeys.all);
    expect(calledKeys).not.toContainEqual(tagsKeys.all);
    expect(calledKeys).not.toContainEqual(searchKeys.all);
  });

  // ── T8: 5 new transcription arms ──────────────────────────────────

  test("TranscriptionStarted seeds the slice as 'running' and invalidates per-file transcripts", async () => {
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
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-001",
          file_uuid: "file-aaa",
          file_name: "vid.mp4",
          queue_size: 1,
        },
      });
    });

    const job = useUiStore.getState().transcription.jobs["req-001"];
    expect(job).toBeDefined();
    expect(job?.file_uuid).toBe("file-aaa");
    expect(job?.file_name).toBe("vid.mp4");
    expect(job?.status.kind).toBe("running");

    expect(invalidateSpy).toHaveBeenCalledWith({
      queryKey: transcriptsKeys.byFileUuid("file-aaa"),
    });
  });

  test("TranscriptionProgress mutates the running status with new processed_ms", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();

    // Seed via Started.
    act(() => {
      sub.fire({
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-002",
          file_uuid: "file-bbb",
          file_name: "clip.wav",
          queue_size: 1,
        },
      });
    });

    act(() => {
      sub.fire({
        kind: "TranscriptionProgress",
        data: { request_uuid: "req-002", processed_ms: 4500, total_ms: 12_000 },
      });
    });

    const status = useUiStore.getState().transcription.jobs["req-002"]?.status;
    expect(status).toEqual({ kind: "running", processed_ms: 4500, total_ms: 12_000 });
  });

  test("TranscriptionCompleted updates status, invalidates queries, and auto-removes after 5s", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();

    act(() => {
      sub.fire({
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-003",
          file_uuid: "file-ccc",
          file_name: "song.mp3",
          queue_size: 1,
        },
      });
    });
    invalidateSpy.mockClear();

    act(() => {
      sub.fire({
        kind: "TranscriptionCompleted",
        data: {
          request_uuid: "req-003",
          transcript_id: "tx-001",
          file_uuid: "file-ccc",
          segment_count: 12,
          language: "en",
        },
      });
    });

    const status = useUiStore.getState().transcription.jobs["req-003"]?.status;
    expect(status).toEqual({
      kind: "completed",
      transcript_id: "tx-001",
      segment_count: 12,
      language: "en",
    });

    // Both invalidations must fire on completion.
    expect(invalidateSpy).toHaveBeenCalledWith({
      queryKey: transcriptsKeys.byFileUuid("file-ccc"),
    });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: searchKeys.all });

    // Job is still present right before the 5s grace window expires.
    await act(async () => { await vi.advanceTimersByTimeAsync(4_999); });
    expect(useUiStore.getState().transcription.jobs["req-003"]).toBeDefined();

    // After 5s, job auto-removes.
    await act(async () => { await vi.advanceTimersByTimeAsync(2); });
    expect(useUiStore.getState().transcription.jobs["req-003"]).toBeUndefined();
  });

  test("TranscriptionCancelled marks slice as cancelled and auto-removes after 3s", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();

    act(() => {
      sub.fire({
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-004",
          file_uuid: "file-ddd",
          file_name: "talk.m4a",
          queue_size: 1,
        },
      });
    });

    act(() => {
      sub.fire({
        kind: "TranscriptionCancelled",
        data: { request_uuid: "req-004" },
      });
    });

    expect(useUiStore.getState().transcription.jobs["req-004"]?.status).toEqual({
      kind: "cancelled",
    });

    await act(async () => { await vi.advanceTimersByTimeAsync(2_999); });
    expect(useUiStore.getState().transcription.jobs["req-004"]).toBeDefined();

    await act(async () => { await vi.advanceTimersByTimeAsync(2); });
    expect(useUiStore.getState().transcription.jobs["req-004"]).toBeUndefined();
  });

  test("TranscriptionFailed marks slice as failed, notifies, and does NOT auto-remove", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();

    act(() => {
      sub.fire({
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-005",
          file_uuid: "file-eee",
          file_name: "broken.mov",
          queue_size: 1,
        },
      });
    });

    act(() => {
      sub.fire({
        kind: "TranscriptionFailed",
        data: {
          request_uuid: "req-005",
          error: { kind: "Auth" },
        },
      });
    });

    const status = useUiStore.getState().transcription.jobs["req-005"]?.status;
    expect(status).toEqual({ kind: "failed", error: { kind: "Auth" } });

    // notifyError fired (one toast in the slice now).
    const notes = useUiStore.getState().notifications;
    expect(notes).toHaveLength(1);
    expect(notes[0]?.kind).toBe("error");
    expect(notes[0]?.message).toMatch(/\[Transcription\]/);

    // Failed jobs persist past every other auto-remove window — let's check
    // 10s in (well beyond the 5s completed grace) and confirm the job is still there.
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(useUiStore.getState().transcription.jobs["req-005"]).toBeDefined();
  });

  test("Unmount clears pending auto-remove timers (no slice mutation post-unmount)", async () => {
    vi.useFakeTimers();
    const sub = createSubscription();
    const queryClient = makeFreshQueryClient();

    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    const { unmount } = renderHook(() => { useDomainEvents(); }, { wrapper });
    await vi.runAllTimersAsync();

    act(() => {
      sub.fire({
        kind: "TranscriptionStarted",
        data: {
          request_uuid: "req-006",
          file_uuid: "file-fff",
          file_name: "x.mp4",
          queue_size: 1,
        },
      });
    });

    act(() => {
      sub.fire({
        kind: "TranscriptionCompleted",
        data: {
          request_uuid: "req-006",
          transcript_id: "tx-006",
          file_uuid: "file-fff",
          segment_count: 1,
          language: null,
        },
      });
    });

    // Job is in the slice; the 5s grace timer is pending.
    expect(useUiStore.getState().transcription.jobs["req-006"]).toBeDefined();

    // Unmount should clear the timer; advancing past 5s must not mutate the slice.
    unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(useUiStore.getState().transcription.jobs["req-006"]).toBeDefined();
  });
});
