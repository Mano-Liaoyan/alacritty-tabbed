#!/usr/bin/env bash
# Report how much of upstream Alacritty's code this fork changes.
#
# Usage: scripts/patch-surface.sh [--base <ref>] [--max-lines <n>]
#
#   --base <ref>     Compare against <ref> instead of the merge base with
#                    upstream/master (or upstream/main).
#   --max-lines <n>  Exit 1 when the lines added and removed in upstream files,
#                    Cargo.lock excluded, exceed <n>.
#
# Upstream files are those present at the base. Files the fork added are only
# counted for information.

set -eu

base=""
max_lines=""

while [ $# -gt 0 ]; do
    case "$1" in
        --base) base="${2:?--base needs a ref}"; shift 2 ;;
        --max-lines) max_lines="${2:?--max-lines needs a number}"; shift 2 ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "patch-surface: unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$max_lines" in
    *[!0-9]*) echo "patch-surface: --max-lines needs a number" >&2; exit 2 ;;
esac

if [ -z "$base" ]; then
    for ref in upstream/master upstream/main; do
        if git rev-parse --verify --quiet "$ref^{commit}" > /dev/null; then
            base=$(git merge-base "$ref" HEAD)
            base_name="merge-base($ref, HEAD)"
            break
        fi
    done
    if [ -z "$base" ]; then
        echo "patch-surface: no upstream/master or upstream/main; run 'git fetch upstream' or pass --base" >&2
        exit 2
    fi
else
    base_name="$base"
    base=$(git rev-parse --verify "$base^{commit}")
fi

# One line per file: "<status> <added> <removed> <path>". Binary files count as 0.
numstat() {
    git -c core.quotepath=false diff --numstat --no-renames --diff-filter="$1" "$base" HEAD \
        | awk -F'\t' -v s="$1" '{ a = ($1 == "-") ? 0 : $1; d = ($2 == "-") ? 0 : $2; print s, a, d, $3 }'
}

# Upstream files: modified or deleted (a rename shows as a deletion plus a new file).
upstream=$( (numstat M; numstat D) | awk 'NF >= 4')
added=$(numstat A | awk 'NF >= 4')

echo "Patch surface against $base_name = $(git rev-parse --short "$base")"
echo
echo "Upstream files changed (M = modified, D = deleted or renamed):"
if [ -n "$upstream" ]; then
    printf '%s\n' "$upstream" | awk '{
        path = $4; for (i = 5; i <= NF; i++) path = path " " $i
        note = (path == "Cargo.lock") ? "  (not counted)" : ""
        printf "  %s %6s %6s  %s%s\n", $1, "+" $2, "-" $3, path, note
    }'
else
    echo "  (none)"
fi
echo

set -- $(printf '%s\n' "$upstream" | awk '
    NF >= 4 {
        path = $4; for (i = 5; i <= NF; i++) path = path " " $i
        if (path == "Cargo.lock") next
        files++; a += $2; d += $3
    }
    END { printf "%d %d %d", files, a, d }')
files=$1 adds=$2 dels=$3
total=$((adds + dels))

set -- $(printf '%s\n' "$added" | awk 'NF >= 4 { files++; a += $2 } END { printf "%d %d", files, a }')
new_files=$1 new_lines=$2

# Hook markers in the modified upstream files, as they are at HEAD.
hooks=$(git -c core.quotepath=false diff --name-only --no-renames --diff-filter=M "$base" HEAD \
    | while IFS= read -r path; do
        git show "HEAD:$path" | grep -c '// tabbed:' || true
    done | awk '{ n += $1 } END { print n + 0 }')

echo "Upstream files changed (Cargo.lock excluded): $files"
echo "Lines changed in upstream files: +$adds -$dels = $total"
echo "'// tabbed:' hook markers in upstream files: $hooks"
echo "New fork files (info only): $new_files files, +$new_lines lines"

if [ -n "$max_lines" ]; then
    echo
    if [ "$total" -gt "$max_lines" ]; then
        echo "Over budget: $total changed lines in upstream files > $max_lines"
        exit 1
    fi
    echo "Within budget: $total <= $max_lines"
fi
