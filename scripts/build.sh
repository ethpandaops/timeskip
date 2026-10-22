#!/usr/bin/env bash
# Overlays the timeskip crate onto a sigp/lighthouse workspace and builds it there.
set -euo pipefail

usage() {
    cat <<EOF
Usage: $0 [--ref REF | --lighthouse-dir DIR] [--prepare-only] [-- CARGO_BUILD_ARGS...]

  --ref REF             sigp/lighthouse branch, tag or full commit SHA to build against,
                        cloned into ./lighthouse (default: \$LIGHTHOUSE_REF or unstable)
  --lighthouse-dir DIR  use an existing Lighthouse checkout as-is; the crate is copied into it
  --prepare-only        overlay the crate without building

Extra arguments after -- go to 'cargo build --release -p timeskip'.
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lighthouse_url="https://github.com/sigp/lighthouse.git"
ref="${LIGHTHOUSE_REF:-unstable}"
lighthouse_dir=""
prepare_only=false
cargo_args=()

while [ $# -gt 0 ]; do
    case "$1" in
        --ref)
            ref="$2"
            shift 2
            ;;
        --lighthouse-dir)
            lighthouse_dir="$2"
            shift 2
            ;;
        --prepare-only)
            prepare_only=true
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --)
            shift
            cargo_args=("$@")
            break
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [ -z "$lighthouse_dir" ]; then
    if [[ ! "$ref" =~ ^[A-Za-z0-9_][A-Za-z0-9._/-]*$ ]]; then
        echo "invalid lighthouse ref: $ref" >&2
        exit 1
    fi
    lighthouse_dir="$repo_root/lighthouse"
    if [ ! -d "$lighthouse_dir/.git" ]; then
        git init -q "$lighthouse_dir"
        git -C "$lighthouse_dir" remote add origin "$lighthouse_url"
    fi
    echo "Fetching sigp/lighthouse@$ref"
    git -C "$lighthouse_dir" fetch -q --depth 1 origin "$ref"
    git -C "$lighthouse_dir" checkout -q --force --detach FETCH_HEAD
fi

if [ ! -f "$lighthouse_dir/Cargo.toml" ]; then
    echo "$lighthouse_dir is not a Lighthouse checkout" >&2
    exit 1
fi
lighthouse_dir="$(cd "$lighthouse_dir" && pwd)"

rm -rf "$lighthouse_dir/timeskip"
cp -R "$repo_root/timeskip" "$lighthouse_dir/timeskip"

manifest="$lighthouse_dir/Cargo.toml"
if ! grep -q '^[[:space:]]*"timeskip",' "$manifest"; then
    if ! grep -q '^members = \[' "$manifest"; then
        echo "no 'members = [' line in $manifest" >&2
        exit 1
    fi
    awk '{ print } /^members = \[/ && !done { print "    \"timeskip\","; done = 1 }' \
        "$manifest" > "$manifest.tmp"
    mv "$manifest.tmp" "$manifest"
fi

# Performance patches are optional: one that no longer applies to this Lighthouse revision is skipped.
for patch in "$repo_root"/patches/*.patch; do
    [ -e "$patch" ] || continue
    name="$(basename "$patch")"
    if git -C "$lighthouse_dir" apply --check "$patch" 2>/dev/null; then
        git -C "$lighthouse_dir" apply "$patch"
        echo "Applied $name"
    elif git -C "$lighthouse_dir" apply --reverse --check "$patch" 2>/dev/null; then
        echo "Already applied $name"
    else
        echo "warning: $name does not apply to this Lighthouse revision, building without it" >&2
    fi
done

revision="$(git -C "$lighthouse_dir" rev-parse --short=9 HEAD 2>/dev/null || echo unknown)"
echo "Overlaid timeskip onto $lighthouse_dir (lighthouse $revision)"

if [ "$prepare_only" = true ]; then
    exit 0
fi

cd "$lighthouse_dir"
cargo build --release -p timeskip ${cargo_args[@]+"${cargo_args[@]}"}
echo "Built ${CARGO_TARGET_DIR:-$lighthouse_dir/target}/release/timeskip"
