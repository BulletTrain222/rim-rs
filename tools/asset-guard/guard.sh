#!/usr/bin/env bash
# Asset guard: refuses RimWorld-owned content, decompiler output and dumps.
#
# Usage:
#   tools/asset-guard/guard.sh staged     # check the index (used by pre-commit)
#   tools/asset-guard/guard.sh history    # audit every blob in every commit
#   tools/asset-guard/guard.sh index      # hash the local game install (optional)
#
# `index` records the git blob id of every file in your local RimWorld install
# into local/research/game-blob-hashes.txt (gitignored). When that file exists,
# `staged` and `history` also reject any blob byte-identical to a game file.
#
# This script is a safety net, not a licence check. A human still has to
# review what gets committed.

set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
HASH_INDEX="$ROOT/local/research/game-blob-hashes.txt"
MAX_BYTES=2097152 # 2 MiB

# Files allowed to mention the signature strings below (they define them).
SELF_PATHS='^(tools/asset-guard/|tools/git-hooks/|\.gitignore$)'

fail=0
report() {
    printf 'asset-guard: %s: %s\n' "$1" "$2" >&2
    fail=1
}

# check_path <path>
check_path() {
    p=$1
    lower=$(printf '%s' "$p" | tr '[:upper:]' '[:lower:]')

    case "$lower" in
        local/*|research/*|research_queue/*|game/*|decompiled/*|rimworld/*|target/*)
            report "$p" "path is inside a local-only directory" ;;
        */data/core/*|*/defs/*|data/*)
            report "$p" "path looks like a copied RimWorld data directory" ;;
        config.toml|*/config.toml)
            report "$p" "local config.toml contains a machine-specific path" ;;
    esac

    case "$lower" in
        *.exe|*.dll|*.pdb|*.so|*.dylib|*.mdb|*.il)
            report "$p" "executable/library" ;;
        *.assets|*.ress|*.resource|*.bundle|*.unity3d|*globalgamemanagers*)
            report "$p" "Unity data container" ;;
        *.cs|*.csproj|*.sln|*.ilspy*|*.dnspy*|*.dotpeek*)
            report "$p" "C# source / decompiler project" ;;
        *.dmp|*.mdmp|*.dump|*.core)
            report "$p" "dump file" ;;
        *.dds|*.psd|*.tga|*.bmp|*.gif|*.webp|*.jpg|*.jpeg)
            report "$p" "image format not allowed" ;;
        *.ogg|*.wav|*.mp3|*.flac|*.ttf|*.otf|*.fnt)
            report "$p" "audio/font file" ;;
        *.tar|*.zip|*.7z|*.rar|*.gz)
            report "$p" "archive" ;;
        *.png)
            case "$p" in
                docs/images/*) ;;
                *) report "$p" "images are only allowed in docs/images/" ;;
            esac ;;
        *.xml)
            case "$p" in
                tests/fixtures/*|crates/*/tests/fixtures/*) ;;
                *) report "$p" "XML is only allowed as synthetic test fixtures" ;;
            esac ;;
    esac
}

# check_blob <blob-id> <path>
BLOB_TMP=$(mktemp)
trap 'rm -f "$BLOB_TMP"' EXIT
check_blob() {
    id=$1
    p=$2

    size=$(git cat-file -s "$id")
    if [ "$size" -gt "$MAX_BYTES" ]; then
        report "$p" "blob is $size bytes (limit $MAX_BYTES)"
        return 0
    fi

    if [ -f "$HASH_INDEX" ] && grep -qx "$id" "$HASH_INDEX"; then
        report "$p" "content is byte-identical to a file in the RimWorld install"
    fi

    # Scan a temporary copy (never pipe into `grep -q`: with pipefail an early
    # match would SIGPIPE the producer and be misread as "no match").
    git cat-file blob "$id" > "$BLOB_TMP"

    case "$p" in
        *.xml)
            if ! grep -q 'synthetic-fixture' "$BLOB_TMP"; then
                report "$p" "XML fixture is missing the 'synthetic-fixture' marker comment"
            fi ;;
    esac

    if printf '%s' "$p" | grep -Eq "$SELF_PATHS"; then
        return 0
    fi
    case "$p" in
        docs/images/*.png) return 0 ;;
    esac
    # Binary = contains NUL bytes.
    if [ "$(tr -cd '\000' < "$BLOB_TMP" | wc -c)" -gt 0 ]; then
        report "$p" "binary content"
        return 0
    fi
    # Signatures of decompiler output and game-owned text. Markdown is exempt
    # because docs legitimately name these tools; review docs by hand.
    case "$p" in
        *.md) return 0 ;;
    esac
    if grep -Eiq 'ILSpy|dnSpy|dotPeek|decompiled with|Assembly-CSharp|namespace (Verse|RimWorld)|Copyright .*Ludeon' "$BLOB_TMP"; then
        report "$p" "contains a decompiler/game signature string"
    fi
}

mode=${1:-staged}
case "$mode" in
    staged)
        # Added/copied/modified/renamed paths in the index (NUL-separated so
        # unusual file names cannot slip through).
        while IFS= read -r -d '' p; do
            check_path "$p"
            check_blob "$(git rev-parse ":$p")" "$p"
        done < <(git diff --cached --name-only --diff-filter=ACMR -z)
        ;;
    history)
        tmp="${TMPDIR:-/tmp}/asset-guard-history.$$"
        # Every (blob, path) pair reachable from any ref, including stashes.
        git rev-list --objects --all --reflog 2>/dev/null |
            git cat-file --batch-check='%(objecttype) %(objectname) %(rest)' |
            awk '$1 == "blob" { $1 = ""; sub(/^ /, ""); print }' > "$tmp"
        count=0
        while IFS=' ' read -r id p; do
            count=$((count + 1))
            check_path "$p"
            check_blob "$id" "$p"
        done < "$tmp"
        rm -f "$tmp"
        printf 'asset-guard: audited %s blob/path pairs across all history\n' "$count" >&2
        ;;
    index)
        game=${2:-}
        if [ -z "$game" ]; then
            echo "usage: $0 index <path-to-RimWorld-install>" >&2
            exit 2
        fi
        mkdir -p "$(dirname "$HASH_INDEX")"
        # Relative paths: native Windows git cannot open MSYS-style /e/... paths.
        (cd "$game" && find . -type f -print | while IFS= read -r f; do
            git hash-object --no-filters -- "$f" 2>/dev/null || {
                echo "asset-guard: skipped unreadable $f" >&2; continue; }
            # Git may normalise CRLF -> LF when staging, which changes the
            # blob id; record the normalised id too (only for files small
            # enough to pass the size limit at all).
            if [ "$(wc -c < "$f")" -le "$MAX_BYTES" ] &&
               [ "$(tr -cd '\r' < "$f" | wc -c)" -gt 0 ]; then
                tr -d '\r' < "$f" | git hash-object --stdin
            fi
        done) | sort -u > "$HASH_INDEX"
        printf 'asset-guard: indexed %s game files into %s\n' \
            "$(wc -l < "$HASH_INDEX" | tr -d ' ')" "$HASH_INDEX" >&2
        exit 0
        ;;
    *)
        echo "usage: $0 staged|history|index <game-path>" >&2
        exit 2
        ;;
esac

if [ "$fail" -ne 0 ]; then
    echo "asset-guard: REFUSED. Remove the offending files (git rm --cached <path>)." >&2
    echo "asset-guard: See docs/provenance.md for what may be published." >&2
    exit 1
fi
echo "asset-guard: OK ($mode)" >&2
