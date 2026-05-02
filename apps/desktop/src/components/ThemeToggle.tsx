/**
 * Three-state theme cycle button. dark → light → system → dark.
 * Icon shows the CURRENT mode; click advances to the next.
 *
 * WHY no useMemo on the icon map: React Compiler 1.0 handles memoization
 * automatically (CLAUDE.md, Batch H — manual useMemo banned).
 */
import { MoonIcon, SunIcon, MonitorIcon } from "@phosphor-icons/react";
import { useTheme, type Theme } from "../lib/theme-provider";

const NEXT: Record<Theme, Theme> = {
  dark: "light",
  light: "system",
  system: "dark",
};

export default function ThemeToggle() {
  const { theme, setTheme } = useTheme();
  const next = NEXT[theme];

  let Icon = MoonIcon;
  let label = "Theme: dark (click for light)";
  if (theme === "light") {
    Icon = SunIcon;
    label = "Theme: light (click for system)";
  } else if (theme === "system") {
    Icon = MonitorIcon;
    label = "Theme: system (click for dark)";
  }

  return (
    <button
      type="button"
      onClick={() => { setTheme(next); }}
      aria-label={label}
      className="inline-flex items-center justify-center rounded-full p-1.5
                 bg-secondary text-secondary-foreground hover:bg-popover
                 transition-colors duration-micro ease-perima
                 focus-visible:outline-none focus-visible:ring-2
                 focus-visible:ring-ring focus-visible:ring-offset-2
                 focus-visible:ring-offset-background"
    >
      <Icon size={18} weight="regular" />
    </button>
  );
}
