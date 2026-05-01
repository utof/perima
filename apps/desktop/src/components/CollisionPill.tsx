/**
 * Collision-group status pill for the StatusBar.
 *
 * WHY warning/success encoding: unverified groups are actionable (they need
 * a human or batch verify); verified groups are informational. Color encodes
 * urgency, not just count. Warning = unverified present, success = all clear.
 *
 * WHY Link to="/dedup": clicking navigates to the dedup management route.
 * The route is registered in router.tsx; TS validates the `to` prop via
 * TanStack Router's `Register` augmentation.
 *
 * WHY VerifiedState is a plain string union (not discriminated object):
 * Rust emits unit enum variants as bare strings under the default serde
 * config (no `#[serde(tag)]`). Pattern-match via ===, not `.kind`.
 */
import { CopyIcon } from "@phosphor-icons/react";
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
      <span className="text-xs text-muted-foreground" title="No candidate duplicates found">
        no candidate duplicates
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

  let label: string;
  if (verified === total) {
    label = "all verified";
  } else if (verified > 0) {
    label = `${total} duplicate${total === 1 ? "" : "s"} (${verified} verified)`;
  } else {
    label = `${total} duplicate${total === 1 ? "" : "s"}`;
  }

  // WHY bg-warning when there are unverified groups, bg-success when all verified:
  // unverified groups are actionable (they need a human or batch verify); verified
  // groups are informational. Color encodes urgency, not just count.
  const bgClass =
    verified === total ? "bg-success text-success-foreground" : "bg-warning text-warning-foreground";

  return (
    <Link
      to="/dedup"
      className={`inline-flex items-center gap-1.5 rounded-full px-3 py-0.5 text-xs font-medium
                  ${bgClass} hover:opacity-90 transition-opacity duration-micro
                  focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring
                  focus-visible:ring-offset-2 focus-visible:ring-offset-background`}
      title="View duplicate groups"
    >
      <CopyIcon size={12} weight="regular" />
      {label}
    </Link>
  );
}
