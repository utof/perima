/**
 * TranscriptionPill — StatusBar widget showing in-flight transcription jobs.
 *
 * 10 scenarios:
 *   1. Renders nothing when zero jobs in slice.
 *   2. Renders pill with count when jobs present.
 *   3. Click opens popover listing file names + statuses.
 *   4. Popover lists all status types (queued/running/completed/cancelled/failed).
 *   5. Cancel button calls api.cancelTranscription for queued/running jobs.
 *   6. Cancel button absent on terminal jobs (completed/cancelled/failed).
 *   7. Failed job tooltip shows error message from coreErrorMessage.
 *   8. Popover closes on ESC keypress.
 *   9. Jobs sorted newest first (descending started_at_ms).
 *  10. Popover hides when jobs empty mid-render.
 *
 * WHY vi.mock("../api"): avoids Tauri IPC in jsdom.
 * WHY seedJob via act(): renderWithProviders resets the store; seeding after
 * render with act() lets the component re-render synchronously.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, screen } from "@testing-library/react";
import { okAsync } from "neverthrow";
import { TranscriptionPill } from "../components/TranscriptionPill";
import * as api from "../api";
import { useUiStore } from "../stores/ui";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { TranscriptionJob } from "../stores/ui";

// ── Mocks ──────────────────────────────────────────────────────────────────────

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    cancelTranscription: vi.fn(),
  };
});

const mockCancelTranscription = vi.mocked(api.cancelTranscription);

// ── Fixtures ───────────────────────────────────────────────────────────────────

function makeJob(overrides: Partial<TranscriptionJob> = {}): TranscriptionJob {
  return {
    request_uuid: "req-0001",
    file_uuid: "file-0001",
    file_name: "vacation.mp4",
    status: { kind: "queued", queue_position: 1 },
    started_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

function seedJob(job: TranscriptionJob) {
  useUiStore.getState().transcription.startJob(job);
}

// ── Setup ──────────────────────────────────────────────────────────────────────

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  mockCancelTranscription.mockReturnValue(okAsync(undefined));
});

// ── Tests ──────────────────────────────────────────────────────────────────────

describe("TranscriptionPill", () => {
  // 1. Hidden when zero jobs
  it("renders nothing when the jobs slice is empty", () => {
    const { container } = renderWithProviders(<TranscriptionPill />);
    // Component returns null — no DOM output.
    expect(container).toBeEmptyDOMElement();
  });

  // 2. Pill with count
  it("renders pill showing 'Transcribing (N)' when N jobs are present", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({ request_uuid: "req-A", file_name: "a.mp4" }));
      seedJob(makeJob({ request_uuid: "req-B", file_name: "b.mp4" }));
    });

    const pill = screen.getByRole("button", { name: /transcribing/i });
    expect(pill.textContent).toMatch(/Transcribing \(2\)/i);
  });

  // 3. Click opens popover with file names + statuses
  it("click opens popover listing job file names", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({ request_uuid: "req-A", file_name: "alpha.mp4" }));
      seedJob(makeJob({ request_uuid: "req-B", file_name: "beta.mp3" }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));

    expect(screen.getByText("alpha.mp4")).toBeInTheDocument();
    expect(screen.getByText("beta.mp3")).toBeInTheDocument();
  });

  // 4. All status types visible in popover
  it("popover renders a badge for each status kind", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({
        request_uuid: "req-queued",
        file_name: "q.mp4",
        status: { kind: "queued", queue_position: 1 },
      }));
      seedJob(makeJob({
        request_uuid: "req-running",
        file_name: "r.mp4",
        status: { kind: "running", processed_ms: 3200, total_ms: 10_000 },
      }));
      seedJob(makeJob({
        request_uuid: "req-completed",
        file_name: "c.mp4",
        status: { kind: "completed", transcript_id: "tx-1", segment_count: 3, language: "en" },
      }));
      seedJob(makeJob({
        request_uuid: "req-cancelled",
        file_name: "x.mp4",
        status: { kind: "cancelled" },
      }));
      seedJob(makeJob({
        request_uuid: "req-failed",
        file_name: "f.mp4",
        status: { kind: "failed", error: { kind: "Auth" } },
      }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));

    // Each status has a visible badge text in the popover.
    expect(screen.getByText(/Queued/i)).toBeInTheDocument();
    expect(screen.getByText(/Transcribing 32%/i)).toBeInTheDocument();
    expect(screen.getByText(/Done!/i)).toBeInTheDocument();
    expect(screen.getByText(/Cancelled/i)).toBeInTheDocument();
    expect(screen.getByText(/Failed/i)).toBeInTheDocument();
  });

  // 5. Cancel button calls api.cancelTranscription for queued + running jobs
  it("Cancel button fires api.cancelTranscription with the job's request_uuid", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({
        request_uuid: "req-cancel-me",
        file_name: "target.mp4",
        status: { kind: "queued", queue_position: 2 },
      }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));

    expect(mockCancelTranscription).toHaveBeenCalledOnce();
    expect(mockCancelTranscription).toHaveBeenCalledWith("req-cancel-me");
  });

  // 6. Cancel absent on terminal jobs
  it("Cancel button is absent for completed, cancelled, and failed jobs", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({
        request_uuid: "req-c",
        file_name: "c.mp4",
        status: { kind: "completed", transcript_id: "tx-1", segment_count: 1, language: null },
      }));
      seedJob(makeJob({
        request_uuid: "req-x",
        file_name: "x.mp4",
        status: { kind: "cancelled" },
      }));
      seedJob(makeJob({
        request_uuid: "req-f",
        file_name: "f.mp4",
        status: { kind: "failed", error: { kind: "Network", data: "ECONNRESET" } },
      }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));

    // No Cancel button should appear for any of the three terminal states.
    expect(screen.queryByRole("button", { name: /cancel/i })).toBeNull();
  });

  // 7. Failed job tooltip shows coreErrorMessage output
  it("Failed job row carries a title with the coreErrorMessage string", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({
        request_uuid: "req-fail",
        file_name: "broken.mp4",
        status: { kind: "failed", error: { kind: "Auth" } },
      }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));

    // coreErrorMessage({ kind: "Transcription", data: { kind: "Auth" } })
    // → "Authentication failed — provider rejected the API key..."
    const failedBadge = screen.getByTitle(/Authentication failed/i);
    expect(failedBadge).toBeInTheDocument();
  });

  // 8. Popover closes on ESC
  it("popover closes when ESC is pressed", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({ request_uuid: "req-esc", file_name: "esc.mp4" }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));
    expect(screen.getByText("esc.mp4")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape", code: "Escape" });

    expect(screen.queryByText("esc.mp4")).toBeNull();
  });

  // 9. Sorted newest first (descending started_at_ms)
  it("popover lists jobs newest-first by started_at_ms", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({
        request_uuid: "req-old",
        file_name: "older.mp4",
        started_at_ms: 1_000_000_000_000,
      }));
      seedJob(makeJob({
        request_uuid: "req-new",
        file_name: "newer.mp4",
        started_at_ms: 2_000_000_000_000,
      }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));

    const items = screen.getAllByTestId("transcription-job-row");
    // First row should be the newer job.
    expect(items[0]).toHaveTextContent("newer.mp4");
    expect(items[1]).toHaveTextContent("older.mp4");
  });

  // 10. Popover hides when jobs become empty mid-render
  it("popover disappears when the jobs slice empties after open", () => {
    renderWithProviders(<TranscriptionPill />);
    act(() => {
      seedJob(makeJob({ request_uuid: "req-gone", file_name: "gone.mp4" }));
    });

    fireEvent.click(screen.getByRole("button", { name: /transcribing/i }));
    expect(screen.getByText("gone.mp4")).toBeInTheDocument();

    // Clear the jobs slice — simulate auto-removal by useDomainEvents.
    act(() => {
      useUiStore.getState().transcription.removeJob("req-gone");
    });

    // Component returns null when jobs is empty; popover (and pill) both gone.
    expect(screen.queryByText("gone.mp4")).toBeNull();
    expect(screen.queryByRole("button", { name: /transcribing/i })).toBeNull();
  });
});
