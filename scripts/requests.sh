#!/bin/sh
# Integration requests, ranked: open issues labelled
# `integration-request`, most 👍 first, then most reactions, then oldest.
# The top of this list is what gets built next. The last
# block prints the `REQUESTED` lines for crates/core/src/connectors.rs, one
# per issue whose title names a registry id, so a Planned row can show its
# issue number.
#
#   sh scripts/requests.sh [--repo owner/name]
set -eu
repo=${2:-james-clarke/chronicle}
[ "${1:-}" = "--repo" ] || repo=james-clarke/chronicle
gh issue list --repo "$repo" --label integration-request --state open --limit 200 \
  --json number,title,createdAt,reactionGroups,body > "${TMPDIR:-/tmp}/requests.$$.json"
f="${TMPDIR:-/tmp}/requests.$$.json"
trap 'rm -f "$f"' EXIT

jq -r '
  def up: [.reactionGroups[]? | select(.content == "THUMBS_UP") | .users.totalCount] | add // 0;
  def all: [.reactionGroups[]? | .users.totalCount] | add // 0;
  def tool: (.body | capture("### Tool\\n\\n(?<t>[^\\n]*)") | .t) // "";
  sort_by([-(up), -(all), .createdAt])
  | (["#", "👍", "all", "tool", "title"] | @tsv),
    (.[] | [.number, up, all, tool, .title] | @tsv)
' "$f" | column -t -s "$(printf '\t')"

echo
echo "REQUESTED lines (title's tool lowercased with spaces as underscores; check the id against the registry):"
jq -r '.[] | "    (\"" + ((.body | capture("### Tool\\n\\n(?<t>[^\\n]*)") | .t) // .title | ascii_downcase | gsub("[^a-z0-9]+"; "_") | gsub("^_|_$"; "")) + "\", " + (.number | tostring) + "),"' "$f"
