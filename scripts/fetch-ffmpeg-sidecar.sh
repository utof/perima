#!/usr/bin/env bash
# Fetch the ffmpeg static binary used by perima-desktop's externalBin
# sidecar slot.
#
# WHY this script (T12, Linux v1):
#   tauri-build validates `bundle.externalBin` paths during EVERY cargo
#   compile of `perima-desktop` (not just `tauri build`). With
#   `externalBin = ["binaries/ffmpeg"]` declared in tauri.conf.json,
#   tauri-build looks for `crates/desktop/binaries/ffmpeg-{target-triple}`
#   and fails the build if the file is absent. Since the binary is
#   ~80 MB and platform-specific, it is gitignored — every dev machine
#   and every CI runner provisions it via this script before any cargo
#   command touches `perima-desktop`.
#
# WHY johnvansickle.com over `ffmpeg-sidecar`'s built-in downloader:
#   The Rust crate's `auto_download` defaults to "latest" build URLs
#   that can change underfoot; pinning a stable johnvansickle release
#   (BtbN's evermeet equivalent) keeps CI reproducible across reruns.
#   Their static builds are widely used in Tauri sidecar deployments
#   (referenced in `ffmpeg-sidecar`'s README + multiple Tauri tutorials).
#
# WHY direct `curl` + `tar` over a Rust helper:
#   Avoids a chicken-and-egg problem — fetching ffmpeg via `cargo run`
#   would compile perima-desktop first, which fails without the file.
#
# Usage:
#   scripts/fetch-ffmpeg-sidecar.sh
#
# Linux only in v1. macOS + Windows tracked as T12 follow-up issues.

set -euo pipefail

# Resolve the repo root from the script location so `just sidecar` and
# direct invocations both work regardless of CWD.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
sidecar_dir="$repo_root/crates/desktop/binaries"

mkdir -p "$sidecar_dir"

# Map host OS -> ({target-triple}, {fetch_command}).
case "$(uname -s)" in
  Linux)
    target_triple="x86_64-unknown-linux-gnu"
    out_path="$sidecar_dir/ffmpeg-$target_triple"
    if [[ -x "$out_path" ]]; then
      echo "ffmpeg sidecar already present at $out_path; skipping fetch."
      exit 0
    fi
    # WHY johnvansickle release-amd64-static: well-known stable static
    # build, no glibc dependency surprises across Ubuntu releases.
    url="https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT
    archive="$tmp_dir/ffmpeg.tar.xz"

    # WHY a hand-rolled retry loop instead of just `curl --retry`:
    # CI fetches this tarball from 4 jobs (3-platform matrix +
    # bindings-drift). johnvansickle.com throttles bursts from a single
    # GitHub Actions egress range, and it does so by returning a SHORT
    # HTML notice with HTTP status 200 — not a 4xx/5xx. `curl --retry`
    # and `--fail` both key off the status code, so curl reports success
    # and the truncated body only explodes later inside tar as
    # "xz: (stdin): File format not recognized" — an opaque message that
    # reads like archive corruption rather than a throttled download.
    # Observed 2026-08-01: the matrix ubuntu job fetched 41 MB fine while
    # bindings-drift got a 0.3-second "download" minutes later.
    # So: validate the payload ourselves, and treat a too-small body as a
    # retryable condition.
    min_bytes=10000000   # real tarball is ~41 MB; anything smaller is a server message
    max_attempts=5
    attempt=1
    while true; do
      echo "Downloading ffmpeg static build from $url (attempt $attempt/$max_attempts) ..."
      # `|| true` so a curl-level failure falls through to the same
      # size check + backoff path rather than tripping `set -e`.
      curl --fail --location --silent --show-error \
        --connect-timeout 30 --output "$archive" "$url" || true

      archive_bytes=0
      [[ -f "$archive" ]] && archive_bytes="$(wc -c < "$archive")"

      if (( archive_bytes >= min_bytes )); then
        break
      fi

      echo "WARNING: got ${archive_bytes} bytes, expected >= ${min_bytes}." >&2
      if [[ -s "$archive" ]]; then
        echo "         First bytes of the response:" >&2
        head -c 200 "$archive" >&2 || true
        echo >&2
      fi

      if (( attempt >= max_attempts )); then
        echo "ERROR: $url did not serve the tarball after $max_attempts attempts." >&2
        echo "       Most likely rate-limited. See GH #183 (sha256-pin + mirror)." >&2
        exit 1
      fi

      backoff=$(( attempt * 10 ))
      echo "         Retrying in ${backoff}s ..." >&2
      sleep "$backoff"
      attempt=$(( attempt + 1 ))
    done

    echo "Extracting ($archive_bytes bytes) ..."
    tar -xJf "$archive" -C "$tmp_dir"
    extracted_bin="$(find "$tmp_dir" -maxdepth 2 -type f -name ffmpeg | head -n1)"
    if [[ -z "$extracted_bin" ]]; then
      echo "ERROR: could not find ffmpeg binary in extracted tarball" >&2
      exit 1
    fi
    cp "$extracted_bin" "$out_path"
    chmod +x "$out_path"
    echo "Installed: $out_path"
    ;;
  Darwin)
    # TODO(T12-followup): macOS sidecar fetch (e.g. evermeet.cx static
    # build) — file an issue at plan-merge. For now, drop a stub so
    # `cargo build -p perima-desktop` succeeds; runtime invocation
    # surfaces a clear error from the audio pipeline.
    target_triple="$(rustc --print host-tuple 2>/dev/null || echo x86_64-apple-darwin)"
    out_path="$sidecar_dir/ffmpeg-$target_triple"
    echo "WARNING: macOS sidecar fetch not implemented (T12-followup);" >&2
    echo "         writing stub at $out_path so tauri-build passes." >&2
    : > "$out_path"
    chmod +x "$out_path"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    # TODO(T12-followup): Windows sidecar fetch (e.g. BtbN GitHub
    # releases) — file an issue at plan-merge.
    target_triple="$(rustc --print host-tuple 2>/dev/null || echo x86_64-pc-windows-msvc)"
    out_path="$sidecar_dir/ffmpeg-$target_triple.exe"
    echo "WARNING: Windows sidecar fetch not implemented (T12-followup);" >&2
    echo "         writing stub at $out_path so tauri-build passes." >&2
    : > "$out_path"
    ;;
  *)
    echo "ERROR: unsupported host OS: $(uname -s)" >&2
    exit 1
    ;;
esac
