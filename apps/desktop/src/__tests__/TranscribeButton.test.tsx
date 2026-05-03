/**
 * TranscribeButton — component tests.
 *
 * 7 scenarios:
 *   1. Idle render — no job in slice → button shows "Transcribe", click calls api.transcribe.
 *   2. Queued render — slice has job with status.kind === "queued" → shows queue position,
 *      click calls api.cancelTranscription.
 *   3. Running render — slice has job with processed_ms/total_ms → shows percent.
 *   4. Running render with no total_ms — shows "..." instead of percent.
 *   5. Completed render — button is disabled, shows "Done!".
 *   6. Failed render — button shows "Retry", has destructive variant, click resets to Idle attempt.
 *   7. api.transcribe error — mock to reject with QueueFull → notifyError called.
 *
 * WHY vi.mock("../api"): api wrappers invoke window.__TAURI__ IPC; mocking avoids
 * wiring a real Tauri context in jsdom.
 *
 * WHY act + startJob (not seedJob before render): renderWithProviders calls
 * resetUiStore() internally, which zeroes the jobs map. Seeding AFTER render
 * via act() lets the component re-render with the seeded state synchronously,
 * without needing a round-trip through waitFor.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { okAsync, errAsync } from "neverthrow";
import { TranscribeButton } from "../components/TranscribeButton";
import * as api from "../api";
import { useUiStore } from "../stores/ui";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { CoreError, TranscribeStartedPayload } from "../bindings";
import type { TranscriptionJob } from "../stores/ui";

// ── Mocks ──────────────────────────────────────────────────────────────────────

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    transcribe: vi.fn(),
    cancelTranscription: vi.fn(),
  };
});

const mockTranscribe = vi.mocked(api.transcribe);
const mockCancelTranscription = vi.mocked(api.cancelTranscription);

// ── Fixtures ───────────────────────────────────────────────────────────────────

const FILE_UUID = "file-uuid-0001";
const FILE_NAME = "vacation.mp4";
const FILE_SOURCE = "videos/vacation.mp4";

const defaultProps = {
  fileUuid: FILE_UUID,
  fileName: FILE_NAME,
  source: FILE_SOURCE,
};

function makeJob(overrides: Partial<TranscriptionJob> = {}): TranscriptionJob {
  return {
    request_uuid: "req-0001",
    file_uuid: FILE_UUID,
    file_name: FILE_NAME,
    status: { kind: "queued", queue_position: 1 },
    started_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

/**
 * Seed a job into the live Zustand store via the slice's own `startJob` action.
 * Must be wrapped in `act()` so React processes the re-render synchronously.
 *
 * WHY startJob (not raw setState with jobs object): `startJob` is the slice's
 * canonical mutator — avoids overwriting action closures while preserving
 * full type safety on the `TranscriptionJob` shape.
 */
function seedJob(job: TranscriptionJob) {
  useUiStore.getState().transcription.startJob(job);
}

// ── Tests ──────────────────────────────────────────────────────────────────────

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
});

describe("TranscribeButton", () => {
  // 1. Idle render
  it("Idle: shows 'Transcribe' and calls api.transcribe on click", async () => {
    const started: TranscribeStartedPayload = { request_uuid: "req-new", queue_position: 1 };
    mockTranscribe.mockReturnValue(okAsync(started));

    renderWithProviders(<TranscribeButton {...defaultProps} />);

    const btn = screen.getByRole("button", { name: /transcribe/i });
    expect(btn).toBeInTheDocument();
    expect(btn).not.toBeDisabled();

    fireEvent.click(btn);

    await waitFor(() => {
      expect(mockTranscribe).toHaveBeenCalledOnce();
    });
    expect(mockTranscribe).toHaveBeenCalledWith({
      fileUuid: FILE_UUID,
      fileName: FILE_NAME,
      source: FILE_SOURCE,
      languageHint: null,
    });
  });

  // 2. Queued render
  it("Queued: shows queue position and calls api.cancelTranscription on click", async () => {
    const job = makeJob({
      request_uuid: "req-q",
      status: { kind: "queued", queue_position: 3 },
    });
    mockCancelTranscription.mockReturnValue(okAsync(undefined));

    renderWithProviders(<TranscribeButton {...defaultProps} />);
    // Seed job AFTER render (renderWithProviders resets the store first).
    act(() => { seedJob(job); });

    const btn = screen.getByRole("button");
    expect(btn.textContent).toMatch(/queued.*3|#3/i);

    fireEvent.click(btn);

    await waitFor(() => {
      expect(mockCancelTranscription).toHaveBeenCalledOnce();
    });
    expect(mockCancelTranscription).toHaveBeenCalledWith("req-q");
  });

  // 3. Running with percent
  it("Running: shows percent when total_ms is present", () => {
    const job = makeJob({
      status: { kind: "running", processed_ms: 3200, total_ms: 10_000 },
    });

    renderWithProviders(<TranscribeButton {...defaultProps} />);
    act(() => { seedJob(job); });

    const btn = screen.getByRole("button");
    // 3200 / 10000 * 100 = 32%
    expect(btn.textContent).toMatch(/32\s*%/);
  });

  // 4. Running without total_ms
  it("Running: shows '...' when total_ms is null", () => {
    const job = makeJob({
      status: { kind: "running", processed_ms: 500, total_ms: null },
    });

    renderWithProviders(<TranscribeButton {...defaultProps} />);
    act(() => { seedJob(job); });

    const btn = screen.getByRole("button");
    expect(btn.textContent).toMatch(/\.\.\./);
    // Should NOT contain a numeric percent value
    expect(btn.textContent).not.toMatch(/\d+\s*%/);
  });

  // 5. Completed render
  it("Completed: button is disabled and shows 'Done!'", () => {
    const job = makeJob({
      status: {
        kind: "completed",
        transcript_id: "tx-001",
        segment_count: 5,
        language: "en",
      },
    });

    renderWithProviders(<TranscribeButton {...defaultProps} />);
    act(() => { seedJob(job); });

    const btn = screen.getByRole("button");
    expect(btn).toBeDisabled();
    expect(btn.textContent).toMatch(/done/i);
  });

  // 6. Failed render
  it("Failed: shows 'Retry', has destructive styling, click calls api.transcribe again", async () => {
    const started: TranscribeStartedPayload = { request_uuid: "req-retry", queue_position: 1 };
    mockTranscribe.mockReturnValue(okAsync(started));

    const job = makeJob({
      request_uuid: "req-fail",
      status: { kind: "failed", error: { kind: "Auth" } },
    });

    renderWithProviders(<TranscribeButton {...defaultProps} />);
    act(() => { seedJob(job); });

    const btn = screen.getByRole("button");
    expect(btn.textContent).toMatch(/retry/i);
    // WHY data-variant check: we assert the destructive variant is applied via
    // data-variant attribute (no shadcn; we carry it explicitly for testability).
    expect(btn.dataset["variant"]).toBe("destructive");
    expect(btn).not.toBeDisabled();

    fireEvent.click(btn);

    await waitFor(() => {
      expect(mockTranscribe).toHaveBeenCalledOnce();
    });
    expect(mockTranscribe).toHaveBeenCalledWith({
      fileUuid: FILE_UUID,
      fileName: FILE_NAME,
      source: FILE_SOURCE,
      languageHint: null,
    });
  });

  // 7. api.transcribe error → notifyError
  it("api.transcribe QueueFull error → notifyError fires with friendly message", async () => {
    const err: CoreError = {
      kind: "Transcription",
      data: { kind: "QueueFull", data: { queued: 5 } },
    };
    mockTranscribe.mockReturnValue(errAsync(err));

    renderWithProviders(<TranscribeButton {...defaultProps} />);

    fireEvent.click(screen.getByRole("button", { name: /transcribe/i }));

    await waitFor(() => {
      expect(mockTranscribe).toHaveBeenCalledOnce();
    });

    // notifyError appends a notification to the store.
    await waitFor(() => {
      const notifications = useUiStore.getState().notifications;
      expect(notifications).toHaveLength(1);
      expect(notifications[0]?.kind).toBe("error");
      // coreErrorMessage surfaces the QueueFull message.
      expect(notifications[0]?.message).toMatch(/queue/i);
    });
  });
});
