/**
 * TranscriptionSlice — unit tests for the in-flight job map.
 *
 * The slice tracks request_uuid → TranscriptionJob; `useDomainEvents` mutates
 * it from `AppEvent::Transcription*` arms. Tests pin the contract:
 *
 *  1. `startJob` adds to the map keyed by `request_uuid`.
 *  2. `updateJob` mutates the status field; missing keys are a no-op.
 *  3. `removeJob` deletes by `request_uuid`.
 *  4. Multiple jobs coexist independently (different request_uuids).
 *
 * WHY no-op on missing-uuid update: the auto-remove timer in `useDomainEvents`
 * (5s for Completed, 3s for Cancelled) can race with a user-initiated
 * `removeJob`; making `updateJob` defensive removes a class of timer-vs-user
 * races without forcing every call site to look up first.
 */
import { describe, expect, test, beforeEach } from "vitest";
import { useUiStore } from "../stores/ui";
import { resetUiStore } from "./test-utils";
import type { TranscriptionJob, TranscriptionJobStatus } from "../stores/ui";

beforeEach(() => {
  resetUiStore();
});

function makeJob(overrides: Partial<TranscriptionJob> = {}): TranscriptionJob {
  return {
    request_uuid: "req-1",
    file_uuid: "file-1",
    file_name: "vacation.mp4",
    status: { kind: "queued", queue_position: 1 },
    started_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

describe("TranscriptionSlice", () => {
  test("initial jobs map is empty", () => {
    expect(useUiStore.getState().transcription.jobs).toEqual({});
  });

  test("startJob adds the job to the map keyed by request_uuid", () => {
    const job = makeJob({ request_uuid: "req-aaa" });
    useUiStore.getState().transcription.startJob(job);
    expect(useUiStore.getState().transcription.jobs).toEqual({ "req-aaa": job });
  });

  test("updateJob mutates the status field on an existing job", () => {
    const job = makeJob({ request_uuid: "req-bbb" });
    useUiStore.getState().transcription.startJob(job);

    const next: TranscriptionJobStatus = {
      kind: "running",
      processed_ms: 500,
      total_ms: 12_000,
    };
    useUiStore.getState().transcription.updateJob("req-bbb", next);

    const updated = useUiStore.getState().transcription.jobs["req-bbb"];
    expect(updated).toBeDefined();
    expect(updated?.status).toEqual(next);
    // Other fields untouched.
    expect(updated?.file_uuid).toBe("file-1");
    expect(updated?.file_name).toBe("vacation.mp4");
    expect(updated?.started_at_ms).toBe(job.started_at_ms);
  });

  test("updateJob walks queued -> running -> completed", () => {
    const job = makeJob({ request_uuid: "req-ccc" });
    useUiStore.getState().transcription.startJob(job);

    useUiStore.getState().transcription.updateJob("req-ccc", {
      kind: "running",
      processed_ms: 0,
      total_ms: null,
    });
    expect(useUiStore.getState().transcription.jobs["req-ccc"]?.status.kind).toBe(
      "running",
    );

    useUiStore.getState().transcription.updateJob("req-ccc", {
      kind: "completed",
      transcript_id: "tx-001",
      segment_count: 7,
      language: "en",
    });
    expect(useUiStore.getState().transcription.jobs["req-ccc"]?.status).toEqual({
      kind: "completed",
      transcript_id: "tx-001",
      segment_count: 7,
      language: "en",
    });
  });

  test("removeJob deletes the entry from the map", () => {
    const job = makeJob({ request_uuid: "req-ddd" });
    useUiStore.getState().transcription.startJob(job);
    expect(useUiStore.getState().transcription.jobs["req-ddd"]).toBeDefined();

    useUiStore.getState().transcription.removeJob("req-ddd");
    expect(useUiStore.getState().transcription.jobs["req-ddd"]).toBeUndefined();
    expect(useUiStore.getState().transcription.jobs).toEqual({});
  });

  test("multiple jobs coexist with different request_uuids", () => {
    const a = makeJob({ request_uuid: "req-A", file_uuid: "file-A" });
    const b = makeJob({ request_uuid: "req-B", file_uuid: "file-B" });
    const c = makeJob({ request_uuid: "req-C", file_uuid: "file-C" });

    useUiStore.getState().transcription.startJob(a);
    useUiStore.getState().transcription.startJob(b);
    useUiStore.getState().transcription.startJob(c);

    const jobs = useUiStore.getState().transcription.jobs;
    expect(Object.keys(jobs).sort()).toEqual(["req-A", "req-B", "req-C"]);
    expect(jobs["req-A"]?.file_uuid).toBe("file-A");
    expect(jobs["req-B"]?.file_uuid).toBe("file-B");
    expect(jobs["req-C"]?.file_uuid).toBe("file-C");

    // Removing one leaves the others intact.
    useUiStore.getState().transcription.removeJob("req-B");
    const after = useUiStore.getState().transcription.jobs;
    expect(Object.keys(after).sort()).toEqual(["req-A", "req-C"]);
  });

  test("updateJob on missing request_uuid is a no-op (does NOT throw)", () => {
    expect(() => {
      useUiStore.getState().transcription.updateJob("req-missing", {
        kind: "running",
        processed_ms: 0,
        total_ms: null,
      });
    }).not.toThrow();
    expect(useUiStore.getState().transcription.jobs).toEqual({});
  });

  test("removeJob on missing request_uuid is a no-op (does NOT throw)", () => {
    expect(() => {
      useUiStore.getState().transcription.removeJob("req-missing");
    }).not.toThrow();
    expect(useUiStore.getState().transcription.jobs).toEqual({});
  });

  test("status discriminants serialise per the discriminated-union contract", () => {
    const failed: TranscriptionJobStatus = {
      kind: "failed",
      error: { kind: "Auth" },
    };
    const cancelled: TranscriptionJobStatus = { kind: "cancelled" };
    const job1 = makeJob({ request_uuid: "req-x", status: failed });
    const job2 = makeJob({ request_uuid: "req-y", status: cancelled });

    useUiStore.getState().transcription.startJob(job1);
    useUiStore.getState().transcription.startJob(job2);

    const got = useUiStore.getState().transcription.jobs;
    expect(got["req-x"]?.status).toEqual(failed);
    expect(got["req-y"]?.status).toEqual(cancelled);
  });
});
