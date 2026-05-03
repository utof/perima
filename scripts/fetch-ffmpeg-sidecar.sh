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
    echo "Downloading ffmpeg static build from $url ..."
    curl --fail --location --silent --show-error \
      --output "$tmp_dir/ffmpeg.tar.xz" "$url"
    echo "Extracting ..."
    tar -xJf "$tmp_dir/ffmpeg.tar.xz" -C "$tmp_dir"
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
