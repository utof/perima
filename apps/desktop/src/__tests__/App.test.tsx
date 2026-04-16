import { render, screen, act } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { Mock } from "vitest";
import App from "../App";

describe("App file-event debounce", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    (invoke as Mock).mockReset();
    (listen as Mock).mockReset();
  });

  test("5 rapid file-events within 300ms trigger at most 1 list_files call", async () => {
    // Initial mount: list_files resolves to empty array.
    (invoke as Mock).mockResolvedValue([]);

    // Capture the handler passed to listen so we can drive it.
    let capturedHandler: ((ev: { payload: unknown }) => void) | null = null;
    (listen as Mock).mockImplementation(async (_event, handler) => {
      capturedHandler = handler;
      return () => {};
    });

    // WHY act() around mount: mount effects schedule async list_files and
    // the subscribeToFileEvents promise, both of which land state updates.
    await act(async () => {
      render(<App />);
      await vi.runAllTicks?.();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    // Ignore the initial list_files call from mount — only count events
    // fired after we start dispatching file-events.
    (invoke as Mock).mockClear();
    (invoke as Mock).mockResolvedValue([]);

    if (!capturedHandler) {
      throw new Error("listen handler was never captured");
    }

    // Fire 5 events synchronously — the debounce timer should coalesce.
    // WHY act(): the captured listener callback schedules a setTimeout that
    // later triggers setState. Wrapping keeps React's act() contract happy
    // even though no state actually updates in this synchronous burst.
    act(() => {
      for (let i = 0; i < 5; i++) {
        capturedHandler!({
          payload: {
            type: "Created",
            path: `file${i}.txt`,
            volume: "00000000-0000-0000-0000-000000000000",
          },
        });
      }
    });

    // Before advancing time, no list_files call should have gone out yet.
    const preCalls = (invoke as Mock).mock.calls.filter(
      ([cmd]) => cmd === "list_files",
    );
    expect(preCalls).toHaveLength(0);

    // Advance past the 300ms debounce window. act() flushes the setState
    // that follows the list_files promise resolving.
    await act(async () => {
      vi.advanceTimersByTime(300);
      // Flush microtasks chained off the setTimeout callback.
      await Promise.resolve();
      await Promise.resolve();
    });

    const postCalls = (invoke as Mock).mock.calls.filter(
      ([cmd]) => cmd === "list_files",
    );
    // Exactly one refresh for 5 rapid events — the whole point of debounce.
    expect(postCalls).toHaveLength(1);
  });

  test("surfaces watcher banner when subscribeToFileEvents fails", async () => {
    (invoke as Mock).mockResolvedValue([]);
    (listen as Mock).mockRejectedValue(new Error("channel closed"));

    render(<App />);
    // Wait for the promise chain in the subscribe effect to run.
    // WHY act(): the rejected promise lands a setState via the .catch.
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    // The banner renders with role="alert". Its text begins with "Watcher:"
    // and includes the wrapped error message.
    expect(
      screen.getByText(/Failed to subscribe to watcher events.*channel closed/),
    ).toBeInTheDocument();
  });
});
