import type { Tag } from "../types";

/** Props for {@link TagChip}. */
interface TagChipProps {
  /** Tag to render. */
  tag: Tag;
  /** Optional callback to remove the tag; when provided, renders an x button. */
  onRemove?: () => void;
}

/**
 * Compute a procedural color index (0..11) from a tag name.
 *
 * WHY byte-sum mod 12: blake3 isn't available in the TS bundle; a simple
 * byte-sum of the UTF-8 encoding collides rarely enough for cosmetic use.
 * Collisions are two tags sharing a chip color — harmless.
 */
function colorIndexFor(name: string): number {
  const bytes = new TextEncoder().encode(name);
  let sum = 0;
  for (const b of bytes) sum = (sum + b) % 256;
  return sum % 12;
}

const CHIP_COLORS = [
  "bg-red-700",
  "bg-orange-700",
  "bg-amber-700",
  "bg-yellow-700",
  "bg-lime-700",
  "bg-green-700",
  "bg-emerald-700",
  "bg-teal-700",
  "bg-cyan-700",
  "bg-sky-700",
  "bg-blue-700",
  "bg-indigo-700",
] as const;

/**
 * A colored pill displaying a tag name, with optional remove button.
 */
export default function TagChip({ tag, onRemove }: TagChipProps) {
  const bg = CHIP_COLORS[colorIndexFor(tag.name)];
  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium rounded-full ${bg} text-white`}
      data-testid="tag-chip"
    >
      <span>{tag.name}</span>
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          aria-label={`Remove ${tag.name}`}
          className="ml-0.5 text-white/80 hover:text-white"
        >
          &times;
        </button>
      )}
    </span>
  );
}
