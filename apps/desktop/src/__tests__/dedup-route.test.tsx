/**
 * `/dedup` route — Task 13 spec §4.6.2 wiring tests.
 *
 * Branches under test:
 *   - 0 groups → empty state ("No candidate duplicates").
 *   - N groups → renders one row per group with file count + size + per-group
 *     "Verify this group" button.
 *   - Click "Verify this group" → mutation called with the group's
 *     `file_uuid`s.
 *   - Click "Verify all (slow)" → mutation called with the union of every
 *     group's `file_uuid`s.
 *   - Cancel button visible only when `useUiStore.verifyBatch` is non-null.
 *   - Cancel click → `cancelVerifyBatch` invoked with the active batch id.
 *
 * WHY mock `@tanstack/react-virtual`: jsdom does not implement layout, so
 * `useVirtualizer` reports `getVirtualItems()` = [] (parent height is 0
 * by default). Test bodies care about wiring + rendering, not the
 * virtualisation internals — we replace `useVirtualizer` with a
 * non-virtualising stub that yields one virtual item per index.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { okAsync } from "neverthrow";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import DedupRoute from "../routes/dedup";
import * as api from "../api";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { BatchHandle, CollisionGroup } from "../bindings";
import { useUiStore } from "../stores/ui";

// ── Mocks ─────────────────────────────────────────────────────────────────────

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    listQuickHashCollisions: vi.fn(),
    computeFullHashBatch: vi.fn(),
    cancelVerifyBatch: vi.fn(),
  };
});

// WHY mock react-virtual: see file header. Stub yields one virtual item per
// index, with synthetic offsets so the route's transform/translate works
// without a real ResizeObserver/layout.
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({
        key: i,
        index: i,
        start: i * 200,
        size: 200,
      })),
    getTotalSize: () => count * 200,
    measureElement: () => undefined,
  }),
}));

const mockListCollisions = vi.mocked(api.listQuickHashCollisions);
const mockComputeBatch = vi.mocked(api.computeFullHashBatch);
const mockCancelBatch = vi.mocked(api.cancelVerifyBatch);

// ── Fixtures ──────────────────────────────────────────────────────────────────

function makeFile(uuid: string, path: string, size = 1_500_000_000) {
  return {
    file_uuid: uuid,
    hash: null,
    size,
    volume_id: "vol-aaaa",
    relative_path: path,
    status: "Active" as const,
    first_seen: "2026-04-01T00:00:00Z",
  };
}

function makeGroup(
  quickHash: string,
  files: ReturnType<typeof makeFile>[],
  verifiedState: CollisionGroup["verified_state"] = "Unverified",
): CollisionGroup {
  return { quick_hash: quickHash, files, verified_state: verifiedState };
}

const fakeHandle: BatchHandle = {
  batch_id: "batch-1234",
  total: 0,
};

// ── Setup ─────────────────────────────────────────────────────────────────────

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  mockListCollisions.mockReturnValue(okAsync<CollisionGroup[]>([]));
  mockComputeBatch.mockReturnValue(okAsync(fakeHandle));
  mockCancelBatch.mockReturnValue(okAsync(undefined));
});

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("DedupRoute", () => {
  it("renders empty state when no candidate groups", async () => {
    renderWithProviders(<DedupRoute />);
    expect(
      await screen.findByTestId("dedup-empty-state"),
    ).toBeInTheDocument();
    expect(screen.getByText(/no candidate duplicates/i)).toBeInTheDocument();
  });

  it("renders one row per group with file count + size + verify button", async () => {
    const groups = [
      makeGroup("aaaa", [
        makeFile("uuid-a1", "Counter-Strike 2/clip.mp4"),
        makeFile("uuid-a2", "old_backup/clip.mp4"),
        makeFile("uuid-a3", ".trash/clip.mp4"),
      ]),
      makeGroup("bbbb", [
        makeFile("uuid-b1", "vacation/IMG_1.heic", 5_400_000_000),
        makeFile("uuid-b2", "vacation_old/IMG_1.heic", 5_400_000_000),
      ]),
    ];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />);

    await screen.findByText(/candidate duplicate groups \(2\)/i);
    const rows = await screen.findAllByTestId("dedup-group-row");
    expect(rows).toHaveLength(2);

    // First group: 3 files, ~1.5GB each.
    expect(rows[0]!.textContent).toMatch(/3 files/i);
    expect(rows[0]!.textContent).toMatch(/GB each/i);

    // Second group: 2 files, ~5.4GB each.
    expect(rows[1]!.textContent).toMatch(/2 files/i);

    // Each row has its own Verify button.
    const verifyBtns = screen.getAllByRole("button", { name: /verify this group/i });
    expect(verifyBtns).toHaveLength(2);
  });

  it("clicking 'Verify this group' calls computeFullHashBatch with that group's uuids", async () => {
    const groups = [
      makeGroup("aaaa", [
        makeFile("uuid-a1", "a.mp4"),
        makeFile("uuid-a2", "b.mp4"),
      ]),
      makeGroup("bbbb", [makeFile("uuid-b1", "c.mp4")]),
    ];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />);
    await screen.findAllByTestId("dedup-group-row");

    const verifyBtns = screen.getAllByRole("button", { name: /verify this group/i });
    fireEvent.click(verifyBtns[0]!);

    await waitFor(() => {
      expect(mockComputeBatch).toHaveBeenCalledTimes(1);
    });
    expect(mockComputeBatch).toHaveBeenCalledWith(["uuid-a1", "uuid-a2"]);
  });

  it("clicking 'Verify all (slow)' calls computeFullHashBatch with the union of uuids", async () => {
    const groups = [
      makeGroup("aaaa", [
        makeFile("uuid-a1", "a.mp4"),
        makeFile("uuid-a2", "b.mp4"),
      ]),
      makeGroup("bbbb", [makeFile("uuid-b1", "c.mp4")]),
    ];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />);
    await screen.findAllByTestId("dedup-group-row");

    fireEvent.click(screen.getByTestId("dedup-verify-all-button"));

    await waitFor(() => {
      expect(mockComputeBatch).toHaveBeenCalledTimes(1);
    });
    expect(mockComputeBatch).toHaveBeenCalledWith([
      "uuid-a1",
      "uuid-a2",
      "uuid-b1",
    ]);
  });

  it("'Verify all' is a no-op when groups is empty (does not call IPC)", async () => {
    renderWithProviders(<DedupRoute />);
    await screen.findByTestId("dedup-empty-state");
    // Verify-all button is hidden in the empty state, so the click fixture
    // simply asserts no IPC fires under the empty branch. (No button = no
    // user action possible; this guards against regressing the empty branch
    // into rendering the button.)
    expect(screen.queryByTestId("dedup-verify-all-button")).toBeNull();
    expect(mockComputeBatch).not.toHaveBeenCalled();
  });

  it("cancel button is hidden when no batch is active", async () => {
    const groups = [makeGroup("aaaa", [makeFile("uuid-a1", "a.mp4")])];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />);
    await screen.findAllByTestId("dedup-group-row");

    expect(screen.queryByTestId("dedup-cancel-button")).toBeNull();
  });

  it("cancel button is visible when verifyBatch slice is set; click fires cancelVerifyBatch", async () => {
    const groups = [makeGroup("aaaa", [makeFile("uuid-a1", "a.mp4")])];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />, {
      initialStoreState: {
        verifyBatch: {
          batchId: "batch-active",
          filesDone: 1,
          filesTotal: 3,
          latestOutcome: null,
        },
      },
    });
    await screen.findAllByTestId("dedup-group-row");

    const cancelBtn = screen.getByTestId("dedup-cancel-button");
    expect(cancelBtn).toBeInTheDocument();

    fireEvent.click(cancelBtn);

    await waitFor(() => {
      expect(mockCancelBatch).toHaveBeenCalledTimes(1);
    });
    expect(mockCancelBatch).toHaveBeenCalledWith("batch-active");
  });

  it("renders progress label from the verifyBatch slice", async () => {
    const groups = [makeGroup("aaaa", [makeFile("uuid-a1", "a.mp4")])];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />, {
      initialStoreState: {
        verifyBatch: {
          batchId: "batch-active",
          filesDone: 7,
          filesTotal: 12,
          latestOutcome: {
            outcome: "Computed",
            data: { file_uuid: "uuid-deadbeef-0000", hash: "abc123" },
          },
        },
      },
    });
    await screen.findAllByTestId("dedup-group-row");

    const label = screen.getByTestId("dedup-progress-label");
    expect(label.textContent).toMatch(/7 of 12 done/i);
    // Last 8 chars of file_uuid should appear (slice prefix).
    expect(label.textContent).toMatch(/uuid-dea/i);
  });

  it("verifyBatch active disables per-group + verify-all buttons", async () => {
    const groups = [makeGroup("aaaa", [makeFile("uuid-a1", "a.mp4")])];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />, {
      initialStoreState: {
        verifyBatch: {
          batchId: "batch-active",
          filesDone: 0,
          filesTotal: 1,
          latestOutcome: null,
        },
      },
    });
    await screen.findAllByTestId("dedup-group-row");

    // Per-group verify button should be disabled while a batch is active.
    // WHY findAllByRole + filter: there are TWO matching buttons in the DOM —
    // the per-group "Verify this group" AND the global "Verify all (slow)".
    // Filter to the per-group button by its accessible name prefix.
    const allBtns = screen.getAllByRole("button");
    const perGroupBtn = allBtns.find((b) => {
      // Match either the idle "Verify this group" or the in-flight "Verifying…",
      // but exclude the global "Verify all" button.
      return /^(verify this group|verifying)/i.test(b.textContent.trim());
    });
    expect(perGroupBtn).toBeDefined();
    expect(perGroupBtn).toBeDisabled();

    const verifyAll = screen.getByTestId("dedup-verify-all-button");
    expect(verifyAll).toBeDisabled();
  });

  it("clearing the verifyBatch slice removes the cancel button", async () => {
    const groups = [makeGroup("aaaa", [makeFile("uuid-a1", "a.mp4")])];
    mockListCollisions.mockReturnValue(okAsync(groups));

    renderWithProviders(<DedupRoute />, {
      initialStoreState: {
        verifyBatch: {
          batchId: "batch-active",
          filesDone: 0,
          filesTotal: 1,
          latestOutcome: null,
        },
      },
    });
    await screen.findAllByTestId("dedup-group-row");
    expect(screen.getByTestId("dedup-cancel-button")).toBeInTheDocument();

    // WHY no rerender(): RTL's rerender strips the QueryClientProvider that
    // renderWithProviders wrapped around the original element. Zustand stores
    // are subscribable — components that read `verifyBatch` via
    // `useUiStore((s) => s.verifyBatch)` re-render automatically when the
    // slice is cleared via the store action below.
    // WHY act(): the setState happens outside an event handler, so React 19
    // requires it be wrapped to silence the act() warning.
    act(() => {
      useUiStore.getState().clearVerifyBatch();
    });

    await waitFor(() => {
      expect(screen.queryByTestId("dedup-cancel-button")).toBeNull();
    });
  });
});
