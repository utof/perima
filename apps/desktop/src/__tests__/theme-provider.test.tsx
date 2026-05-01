import { describe, test, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act } from "@testing-library/react";
import { ThemeProvider, useTheme } from "../lib/theme-provider";

const ThemeProbe = () => {
  const { theme, effectiveTheme, setTheme } = useTheme();
  return (
    <div>
      <span data-testid="theme">{theme}</span>
      <span data-testid="effective">{effectiveTheme}</span>
      <button onClick={() => { setTheme("light"); }}>set-light</button>
      <button onClick={() => { setTheme("dark"); }}>set-dark</button>
      <button onClick={() => { setTheme("system"); }}>set-system</button>
    </div>
  );
};

describe("ThemeProvider", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    // WHY matchMedia mock: jsdom does not implement window.matchMedia by
    // default; ThemeProvider's useEffect calls it for system-pref detection.
    // Without this mock, every test would throw "matchMedia is not a function".
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: vi.fn().mockImplementation((query: string) => ({
        matches: false, // default to NOT-light → effectiveTheme = "dark" in system mode
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),    // legacy
        removeListener: vi.fn(), // legacy
        dispatchEvent: vi.fn(),
      })),
    });
  });

  afterEach(() => {
    document.documentElement.removeAttribute("data-theme");
  });

  test("defaults to system mode when no localStorage value", () => {
    const { getByTestId } = render(
      <ThemeProvider><ThemeProbe /></ThemeProvider>,
    );
    expect(getByTestId("theme").textContent).toBe("system");
  });

  test("setTheme('light') sets data-theme=light on <html>", () => {
    const { getByText } = render(
      <ThemeProvider><ThemeProbe /></ThemeProvider>,
    );
    act(() => { getByText("set-light").click(); });
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  test("setTheme('dark') removes data-theme attr", () => {
    const { getByText } = render(
      <ThemeProvider><ThemeProbe /></ThemeProvider>,
    );
    act(() => { getByText("set-light").click(); });
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    act(() => { getByText("set-dark").click(); });
    expect(document.documentElement.getAttribute("data-theme")).toBe(null);
  });

  test("setTheme persists to localStorage under perima-theme key", () => {
    const { getByText } = render(
      <ThemeProvider><ThemeProbe /></ThemeProvider>,
    );
    act(() => { getByText("set-light").click(); });
    expect(localStorage.getItem("perima-theme")).toBe("light");
  });

  test("reads stored theme on mount", () => {
    localStorage.setItem("perima-theme", "light");
    const { getByTestId } = render(
      <ThemeProvider><ThemeProbe /></ThemeProvider>,
    );
    expect(getByTestId("theme").textContent).toBe("light");
    expect(getByTestId("effective").textContent).toBe("light");
  });

  test("useTheme outside provider throws", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(() => render(<ThemeProbe />)).toThrow(
      /useTheme must be used within <ThemeProvider>/,
    );
    spy.mockRestore();
  });
});
