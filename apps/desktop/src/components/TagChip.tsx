import { XIcon } from "@phosphor-icons/react";
import type { Tag } from "../bindings";

/** Props for {@link TagChip}. */
interface TagChipProps {
  /** Tag to render. */
  tag: Tag;
  /** Optional callback to remove the tag; when provided, renders an X button. */
  onRemove?: () => void;
}

/**
 * A muted-background pill displaying a tag name, with an optional X icon
 * for removal. Tag identity is conveyed by the name text — chip color is
 * uniform across all tags by design (design-system-v1).
 */
export default function TagChip({ tag, onRemove }: TagChipProps) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded-full bg-muted text-foreground px-3 py-0.5 text-sm"
      data-testid="tag-chip"
    >
      <span>{tag.name}</span>
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          aria-label={`Remove ${tag.name}`}
          className="inline-flex items-center justify-center rounded-full text-muted-foreground hover:text-foreground transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <XIcon size={12} weight="bold" />
        </button>
      )}
    </span>
  );
}
