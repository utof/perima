/**
 * Collision-group status pill for the StatusBar.
 *
 * Color states per spec §4.6.1:
 * - 0 groups: gray, "no candidate duplicates"
 * - N groups, 0 verified: blue, "N candidate group(s)"
 * - N groups, 0 less than M less than N verified: blue, "N candidate (M ✓)"
 * - all verified: green, "all verified ✓"
 * - errored greater than 0: yellow, "verify error" (reserved for future batch)
 *
 * WHY gray-by-default for 0 groups: collisions are expected to be
 * absent in a well-managed library; green "all clear" would be noisy
 * and draws attention away from actionable states.
 *
 * WHY Link to="/dedup": clicking navigates to the dedup management
 * route (Task 13). The route is registered in router.tsx; TS validates
 * the `to` prop via TanStack Router's `Register` augmentation.
 *
 * WHY VerifiedState is a plain string union (not discriminated object):
 * Rust emits unit enum variants as bare strings under the default serde
 * config (no `#[serde(tag)]`). Pattern-match via ===, not `.kind`.
 */
import { Link } from "@tanstack/react-router";
import type { CollisionGroup } from "../bindings";

interface Props {
  /** Collision groups returned by `list_quick_hash_collisions`. */
  groups: CollisionGroup[];
}

/**
 * Pill component showing duplicate-group status. Clickable → `/dedup`.
 */
export default function CollisionPill({ groups }: Props) {
  const total = groups.length;

  if (total === 0) {
    return (
      <span className="text-gray-500" title="No candidate duplicates found">
        ⊜ no candidate duplicates
      </span>
    );
  }

  // WHY VerifiedDuplicate + VerifiedDistinct both count as "verified":
  // either outcome means a human (or automated check) has resolved the
  // group — Unverified and Mixed still need attention.
  const verified = groups.filter(
    (g) =>
      g.verified_state === "VerifiedDuplicate" ||
      g.verified_state === "VerifiedDistinct",
  ).length;

  let colorClass = "text-blue-400";
  let label: string;

  if (verified === total) {
    colorClass = "text-green-400";
    label = "⊜ all verified ✓";
  } else if (verified > 0) {
    label = `⊜ ${total} candidate${total === 1 ? "" : "s"} (${verified} ✓)`;
  } else {
    label = `⊜ ${total} candidate group${total === 1 ? "" : "s"}`;
  }

  return (
    <Link
      to="/dedup"
      className={`${colorClass} hover:underline`}
      title="View duplicate groups"
    >
      {label}
    </Link>
  );
}
