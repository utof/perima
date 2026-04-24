/**
 * ScanButton — owns its own scan mutation post-Batch-H Task 8a.
 *
 * Behaviours under test:
 *   - renders the trigger label.
 *   - dialog cancel (open returns null) does NOT call api.scan.
 *   - happy path: dialog → api.scan → store mutations + invalidateQueries +
 *     api.startWatch dispatched on success.
 *   - error path: api.scan rejects → notifyError + status reverted to "idle".
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { okAsync, errAsync } from "neverthrow";
import { open as dialogOpen } from "@tauri-apps/plugin-dialog";
import ScanButton from "../components/ScanButton";
import * as api from "../api";
import { useUiStore } from "../stores/ui";
import { filesKeys } from "../queries/files";
import { tagsKeys } from "../queries/tags";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { CoreError, ScanReport } from "../bindings";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    scan: vi.fn(),
    startWatch: vi.fn(),
  };
});

const mockOpen = vi.mocked(dialogOpen);
const mockScan = vi.mocked(api.scan);
const mockStartWatch = vi.mocked(api.startWatch);

const mockReport: ScanReport = {
  files_seen: 10,
  files_new: 3,
  files_updated: 7,
  files_errored: 0,
  bytes_hashed: 1024,
  duration_ms: 42,
  interrupted: false,
  volume_label: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  mockStartWatch.mockReturnValue(okAsync(undefined));
});

describe("ScanButton", () => {
  it("renders the Scan folder button", () => {
    renderWithProviders(<ScanButton />);
    expect(
      screen.getByRole("button", { name: /scan folder/i }),
    ).toBeInTheDocument();
  });

  it("disables button + shows 'Scanning…' when scan.status === 'scanning'", () => {
    renderWithProviders(<ScanButton />, {
      initialStoreState: { scan: { status: "scanning", lastReport: null } },
    });
    const btn = screen.getByRole("button");
    expect(btn).toBeDisabled();
    expect(btn.textContent).toMatch(/scanning/i);
  });

  it("dialog cancel (returns null) does NOT call api.scan", async () => {
    mockOpen.mockResolvedValue(null);
    renderWithProviders(<ScanButton />);

    fireEvent.click(screen.getByRole("button", { name: /scan folder/i }));

    await waitFor(() => {
      expect(mockOpen).toHaveBeenCalledWith({
        directory: true,
        multiple: false,
      });
    });
    expect(mockScan).not.toHaveBeenCalled();
    expect(useUiStore.getState().scan.status).toBe("idle");
  });

  it("on click → dialog → scan → updates store + invalidates queries + starts watch", async () => {
    mockOpen.mockResolvedValue("/home/user/photos");
    mockScan.mockReturnValue(okAsync(mockReport));

    const { queryClient } = renderWithProviders(<ScanButton />);
    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    fireEvent.click(screen.getByRole("button", { name: /scan folder/i }));

    await waitFor(() => {
      expect(mockScan).toHaveBeenCalledWith("/home/user/photos", false);
    });

    await waitFor(() => {
      expect(useUiStore.getState().scan.status).toBe("done");
    });

    const state = useUiStore.getState();
    expect(state.scan.lastReport).toEqual(mockReport);

    // notify("info", "Scanned 10 files") landed.
    expect(state.notifications).toHaveLength(1);
    expect(state.notifications[0]?.kind).toBe("info");
    expect(state.notifications[0]?.message).toMatch(/scanned 10 files/i);

    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: filesKeys.all });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: tagsKeys.all });

    // Watcher auto-start fires after the success path.
    expect(mockStartWatch).toHaveBeenCalledWith("/home/user/photos");
  });

  it("on scan failure → status returns to 'idle' + notifyError fires", async () => {
    mockOpen.mockResolvedValue("/home/user/bad");
    const err: CoreError = { kind: "Internal", data: "disk read failure" };
    mockScan.mockReturnValue(errAsync<ScanReport, CoreError>(err));

    renderWithProviders(<ScanButton />);

    fireEvent.click(screen.getByRole("button", { name: /scan folder/i }));

    await waitFor(() => {
      expect(useUiStore.getState().scan.status).toBe("idle");
    });

    const notes = useUiStore.getState().notifications;
    expect(notes).toHaveLength(1);
    expect(notes[0]?.kind).toBe("error");
    expect(notes[0]?.message).toMatch(/disk read failure/);

    // No invalidation or watcher start on error.
    expect(mockStartWatch).not.toHaveBeenCalled();
  });
});
