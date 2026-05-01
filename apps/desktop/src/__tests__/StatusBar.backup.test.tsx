/**
 * StatusBar Backup button — Task 9 of the database-backup slice.
 *
 * Branches under test:
 *   - Click Backup → api.backupDatabase resolves Ok → info toast with
 *     absolute_path and formatted MB.
 *   - Click Backup → api.backupDatabase resolves Err(BackupFailed/TargetExists)
 *     → error toast that includes "already exists".
 *
 * WHY mock `../api` (not `@tauri-apps/api/core`): the perima frontend test
 * pattern mocks the API layer with `vi.importActual` partial spread, returning
 * `okAsync`/`errAsync` from neverthrow.  Mocking `invoke` directly bypasses
 * `fromInvoke`'s `parseCoreError` and produces a different shape than
 * `useBackupDatabase`'s `.match(...)` branch consumes.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { errAsync, okAsync } from "neverthrow";
import StatusBar from "../components/StatusBar";
import * as api from "../api";
import { useUiStore } from "../stores/ui";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { CoreError } from "../bindings";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    backupDatabase: vi.fn(),
    // WHY listQuickHashCollisions: StatusBar renders <CollisionPill> which
    // calls useCollisions() → listQuickHashCollisions.  Without a mock the
    // real `invoke` fires, which fails in jsdom.  Return an empty list so the
    // pill renders in its neutral "0 groups" state without affecting the
    // backup-button assertions.
    listQuickHashCollisions: vi.fn(),
  };
});

const mockBackupDatabase = vi.mocked(api.backupDatabase);
const mockListCollisions = vi.mocked(api.listQuickHashCollisions);

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  mockListCollisions.mockReturnValue(okAsync([]));
});

describe("StatusBar Backup button", () => {
  it("notifies success with path + MB on Ok", async () => {
    mockBackupDatabase.mockReturnValueOnce(
      okAsync({
        absolute_path: "/tmp/backup.sqlite",
        size_bytes: 1024 * 1024 * 5, // 5 MB
      }),
    );

    renderWithProviders(<StatusBar />);

    fireEvent.click(screen.getByRole("button", { name: /backup/i }));

    await waitFor(() => {
      const notifications = useUiStore.getState().notifications;
      expect(
        notifications.some(
          (n) =>
            n.kind === "info" &&
            n.message.includes("/tmp/backup.sqlite") &&
            n.message.includes("5.0 MB"),
        ),
      ).toBe(true);
    });
  });

  it("notifies typed error on TargetExists", async () => {
    const err: CoreError = {
      kind: "BackupFailed",
      data: {
        reason: {
          kind: "TargetExists",
          data: { path: "/tmp/backup.sqlite" },
        },
      },
    };
    mockBackupDatabase.mockReturnValueOnce(errAsync(err));

    renderWithProviders(<StatusBar />);

    fireEvent.click(screen.getByRole("button", { name: /backup/i }));

    await waitFor(() => {
      const notifications = useUiStore.getState().notifications;
      expect(
        notifications.some(
          (n) => n.kind === "error" && n.message.includes("already exists"),
        ),
      ).toBe(true);
    });
  });
});
