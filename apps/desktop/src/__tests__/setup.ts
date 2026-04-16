import { vi } from "vitest";
import "@testing-library/jest-dom";

// WHY: Tauri's invoke requires the native runtime; in jsdom it would throw.
// Mock the entire module so every test file gets predictable stubs.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// WHY: The dialog plugin opens a native OS dialog which is unavailable in jsdom.
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

// WHY: `@tauri-apps/api/event::listen` hits the Tauri runtime (WebView IPC).
// The default mock returns an unsubscribe stub; individual tests override
// it via `(listen as Mock).mockImplementation(...)` to capture the handler.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
