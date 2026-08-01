/**
 * Library-health control: run a verify sweep, and remove entries for
 * files that are gone.
 *
 * WHY the two actions are separate buttons rather than one "clean up":
 * verify is safe and repeatable; prune deletes catalogue rows. Fusing
 * them would mean a single click both decides what is missing and acts
 * on that decision, with no moment for the user to look at the number
 * first. Keeping them apart is what makes the count reviewable.
 *
 * WHY prune is hidden entirely at zero rather than shown disabled: a
 * greyed "Remove 0 missing" invites clicking to find out what it does.
 * The control appears when there is something to remove.
 */

import { useState } from "react";

import {
  useMissingCount,
  usePruneMissing,
  useVerifyLocations,
} from "../queries/verify";

const BTN =
  "rounded-full px-3 py-1 text-sm font-medium transition-colors duration-micro " +
  "ease-perima focus-visible:outline-none focus-visible:ring-2 " +
  "focus-visible:ring-ring focus-visible:ring-offset-2 " +
  "focus-visible:ring-offset-background disabled:opacity-40 " +
  "disabled:pointer-events-none";

export default function LibraryHealthPill() {
  const { data: missing = 0 } = useMissingCount();
  const verify = useVerifyLocations();
  const prune = usePruneMissing();
  const [confirming, setConfirming] = useState(false);

  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        onClick={() => {
          verify.mutate(false);
        }}
        disabled={verify.isPending}
        title="Check whether every indexed file is still on disk"
        className={`${BTN} bg-secondary text-secondary-foreground hover:bg-popover`}
      >
        {verify.isPending ? "Checking…" : "Check files"}
      </button>

      {missing > 0 &&
        (confirming ? (
          <span className="flex items-center gap-2">
            {/* WHY the count is repeated in the confirm: the user may have
                run verify some time ago, and the number is the whole
                basis for the decision. */}
            <span className="text-xs text-muted-foreground">
              Remove {missing} missing file(s) from the library?
            </span>
            <button
              type="button"
              onClick={() => {
                prune.mutate(false);
                setConfirming(false);
              }}
              disabled={prune.isPending}
              className={`${BTN} bg-destructive text-destructive-foreground hover:opacity-90`}
            >
              {prune.isPending ? "Removing…" : "Remove"}
            </button>
            <button
              type="button"
              onClick={() => {
                setConfirming(false);
              }}
              className={`${BTN} bg-secondary text-secondary-foreground hover:bg-popover`}
            >
              Cancel
            </button>
          </span>
        ) : (
          <button
            type="button"
            onClick={() => {
              setConfirming(true);
            }}
            title="Remove catalogue entries for files that are no longer on disk. The files themselves are not touched."
            className={`${BTN} bg-destructive/15 text-destructive-foreground ring-1 ring-destructive/40 hover:bg-destructive/25`}
          >
            {missing} missing
          </button>
        ))}
    </div>
  );
}
