/**
 * Type-scale playground. Hidden route; no nav link; navigate manually
 * via #/type. Survives shipping — useful for designer iteration, font
 * swaps, color tweaks.
 *
 * WHY enumerated swatch class strings (not `bg-${name}` template literals):
 * Tailwind v4's JIT scans source for full class-name strings at build time.
 * Dynamic `bg-${name}` produces no utilities. Each SWATCHES entry pairs the
 * variable name (for the label) with the literal utility class so the JIT
 * picks it up.
 *
 * WHY border / input / ring rendered as swatches alongside surface colors:
 * these tokens are alpha-low hairlines (rgba(255,255,255,0.08) in dark) so
 * the swatches will look near-invisible against bg-card border chrome.
 * Intentional — it's a designer-debugging surface, not production UI;
 * seeing them disappear confirms the alpha is at the intended low value.
 */
import { useTheme } from "../lib/theme-provider";

const SCALE = [
  { cls: "display-xl", label: ".display-xl", text: "Painless library", meta: "Fraunces / 4.5rem / opsz 144" },
  { cls: "h1",         label: ".h1",         text: "Your archive at peace", meta: "Fraunces / 3.5rem / opsz 96" },
  { cls: "h2",         label: ".h2",         text: "Section title", meta: "Fraunces / 2.5rem / opsz 48" },
  { cls: "h3",         label: ".h3",         text: "Card title", meta: "Fraunces / 1.75rem / opsz 36" },
  { cls: "h4",         label: ".h4",         text: "Subheading", meta: "Inter / 1.25rem" },
  { cls: "",           label: "body",        text: "The quick brown fox jumps over the lazy dog.", meta: "Inter / 1rem / 1.55" },
  { cls: "caption",    label: ".caption",    text: "Image caption", meta: "Inter / 0.875rem" },
  { cls: "eyebrow",    label: ".eyebrow",    text: "Section label", meta: "Inter / 0.75rem / uppercase" },
  { cls: "mono-metadata", label: ".mono-metadata", text: "/Users/perima/library/photo.jpg", meta: "JetBrains Mono / 0.875rem" },
];

const SWATCHES: ReadonlyArray<{ name: string; cls: string }> = [
  { name: "background",             cls: "bg-background" },
  { name: "foreground",             cls: "bg-foreground" },
  { name: "card",                   cls: "bg-card" },
  { name: "card-foreground",        cls: "bg-card-foreground" },
  { name: "popover",                cls: "bg-popover" },
  { name: "popover-foreground",     cls: "bg-popover-foreground" },
  { name: "muted",                  cls: "bg-muted" },
  { name: "muted-foreground",       cls: "bg-muted-foreground" },
  { name: "primary",                cls: "bg-primary" },
  { name: "primary-foreground",     cls: "bg-primary-foreground" },
  { name: "secondary",              cls: "bg-secondary" },
  { name: "secondary-foreground",   cls: "bg-secondary-foreground" },
  { name: "accent",                 cls: "bg-accent" },
  { name: "accent-foreground",      cls: "bg-accent-foreground" },
  { name: "destructive",            cls: "bg-destructive" },
  { name: "destructive-foreground", cls: "bg-destructive-foreground" },
  { name: "success",                cls: "bg-success" },
  { name: "warning",                cls: "bg-warning" },
  { name: "info",                   cls: "bg-info" },
  { name: "border",                 cls: "bg-border" },
  { name: "input",                  cls: "bg-input" },
  { name: "ring",                   cls: "bg-ring" },
];

const OPSZ = [9, 36, 48, 96, 144];
const WEIGHTS = [400, 500, 600];

export default function TypeRoute() {
  const { theme, effectiveTheme } = useTheme();
  return (
    <div className="bg-background text-foreground p-8 min-h-screen overflow-y-auto">
      <header className="mb-12">
        <h1 className="h1 mb-2">Type playground</h1>
        <p className="caption text-muted-foreground">
          Theme: {theme} (resolved: {effectiveTheme}). Hidden route — not linked from nav.
        </p>
      </header>

      <section className="mb-12">
        <h2 className="h3 mb-6">Type scale</h2>
        <div className="space-y-6">
          {SCALE.map((row) => (
            <div key={row.label} className="flex items-baseline gap-6 border-b border-border pb-4">
              <span className="mono-metadata text-muted-foreground w-40 shrink-0">{row.label}</span>
              <span className={`flex-1 ${row.cls}`}>{row.text}</span>
              <span className="mono-metadata text-muted-foreground text-xs w-64 shrink-0 text-right">{row.meta}</span>
            </div>
          ))}
        </div>
      </section>

      <section className="mb-12">
        <h2 className="h3 mb-6">Fraunces opsz × weight grid</h2>
        <div className="grid grid-cols-5 gap-4">
          {WEIGHTS.flatMap((w) =>
            OPSZ.map((opsz) => (
              <div key={`${w}-${opsz}`} className="bg-card rounded-md p-4 border border-border">
                <div
                  className="text-2xl"
                  style={{
                    fontFamily: "var(--font-display)",
                    fontWeight: w,
                    fontVariationSettings: `"opsz" ${opsz}`,
                  }}
                >
                  Aa
                </div>
                <div className="mono-metadata text-muted-foreground text-xs mt-2">w{w} / opsz {opsz}</div>
              </div>
            )),
          )}
        </div>
      </section>

      <section className="mb-12">
        <h2 className="h3 mb-6">Italic specimens</h2>
        <p
          className="text-3xl"
          style={{
            fontFamily: "var(--font-display)",
            fontStyle: "italic",
            fontVariationSettings: `"opsz" 96`,
          }}
        >
          A library of moments, kept calmly.
        </p>
      </section>

      <section className="mb-12">
        <h2 className="h3 mb-6">Spectrum</h2>
        <h2 className="text-spectrum h2 mb-6">Perima Spectrum</h2>
        <div className="bg-spectrum h-24 rounded-md mb-3" />
        <div className="bg-spectrum w-16 h-16 rounded-full" aria-label="Petal mark preview" />
      </section>

      <section>
        <h2 className="h3 mb-6">Theme palette</h2>
        <div className="grid grid-cols-4 gap-4">
          {SWATCHES.map(({ name, cls }) => (
            <div key={name} className="rounded-md border border-border overflow-hidden">
              <div className={`h-16 ${cls}`} />
              <div className="p-2 mono-metadata text-xs">--{name}</div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
