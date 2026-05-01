import { describe, test, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { ThemeProvider } from "../lib/theme-provider";
import ThemeToggle from "../components/ThemeToggle";

describe("ThemeToggle", () => {
  beforeEach(() => {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
    });
  });

  test("renders Moon icon initially (dark mode default for tests)", () => {
    localStorage.clear();
    localStorage.setItem("perima-theme", "dark");
    const { container } = render(
      <ThemeProvider><ThemeToggle /></ThemeProvider>,
    );
    const btn = container.querySelector("button");
    expect(btn).not.toBeNull();
    expect(btn!.getAttribute("aria-label")).toMatch(/dark/i);
  });

  test("clicking cycles dark -> light -> system -> dark", () => {
    localStorage.clear();
    localStorage.setItem("perima-theme", "dark");
    const { container } = render(
      <ThemeProvider><ThemeToggle /></ThemeProvider>,
    );
    const btn = container.querySelector("button")!;

    fireEvent.click(btn);
    expect(btn.getAttribute("aria-label")).toMatch(/light/i);

    fireEvent.click(btn);
    expect(btn.getAttribute("aria-label")).toMatch(/system/i);

    fireEvent.click(btn);
    expect(btn.getAttribute("aria-label")).toMatch(/dark/i);
  });
});
