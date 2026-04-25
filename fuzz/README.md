# perima-fuzz

cargo-fuzz / libFuzzer targets for `perima-hash` (BLAKE3 streaming wrapper) and `perima-media` (EXIF/JPEG metadata extraction).

## Quick start (local)

Requires `rustup` + nightly toolchain (auto-installed via `rust-toolchain.toml` on first invocation) + the `cargo-fuzz` CLI:

```bash
cargo install --locked cargo-fuzz   # one-time per dev box
cd fuzz
cargo fuzz list                     # → exif, blake3
cargo fuzz run exif -- -max_total_time=60
cargo fuzz run blake3 -- -max_total_time=60
```

Crashes land in `fuzz/artifacts/<target>/crash-<hash>`. Reproduce with:

```bash
cargo fuzz run <target> artifacts/<target>/crash-<hash>
```

Minimise a reproducer with `cargo fuzz tmin`:

```bash
cargo fuzz tmin <target> artifacts/<target>/crash-<hash>
```

## Toolchain isolation

`fuzz/rust-toolchain.toml` pins nightly. The `[workspace] members = []` line in `fuzz/Cargo.toml` declares this as a leaf workspace independent of the parent — production builds stay on stable.

## Triage flow

When a crash is found in CI:

1. Download the `fuzz-artifacts-<target>` artifact from the workflow run.
2. Reproduce locally with the commands above.
3. **Distinguish:**
   - **Panic in our wrapper code** (`perima_hash` or `perima_media`): fix here. Add the minimised reproducer as a regression test in the appropriate `crates/<x>/tests/` file before fixing.
   - **Panic in nom-exif / image / blake3 / mp4parse**: file an upstream issue with the reproducer. Optionally add a defensive guard in our wrapper if the upstream fix is blocking.
4. Drop a comment on the GH baseline-anchor issue with the finding + reproducer link.

## CI cadence

`.github/workflows/fuzz.yml` runs Mondays 06:00 UTC (cron) + on manual `workflow_dispatch` (with optional `max_total_time` input). Linux-only. Observability-only — crashes do not block the workflow; reviewers act on artifact contents.
