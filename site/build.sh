#!/bin/sh
# Bakes the repository's git pulse into site/index.html and checks the page
# against its byte budget. Render runs it as the build command on every push
# (in place). Locally, `sh site/build.sh DIR` bakes a copy of site/ into DIR
# and leaves the committed placeholders alone.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
site=$root/site
if [ $# -gt 0 ]; then
  mkdir -p "$1" && cp -R "$site/." "$1" && site=$(cd "$1" && pwd)
fi
html=$site/index.html
tmp=${TMPDIR:-/tmp}/chronicle-site-build.$$
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT
cd "$root"

esc() { sed -e 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g'; }

# Replace the lines between <!-- NAME --> and <!-- /NAME --> with a file's content.
splice() {
  awk -v name="$1" -v src="$2" '
    $0 ~ "<!-- " name " -->" { print; while ((getline line < src) > 0) print line; close(src); skip = 1; next }
    $0 ~ "<!-- /" name " -->" { skip = 0 }
    !skip { print }
  ' "$html" > "$tmp/html" && cat "$tmp/html" > "$html"
}

# --- git history ------------------------------------------------------------

# Render's clone is shallow and its origin cannot fetch the private repo, so
# the unshallow goes over a token when one is set (a fine-grained PAT with
# read on Contents, GITHUB_TOKEN in the Render environment). The token never
# reaches the log: git's errors are only echoed for the plain-origin attempt.
if [ "$(git rev-parse --is-shallow-repository 2>/dev/null)" = true ]; then
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    src="https://x-access-token:$GITHUB_TOKEN@github.com/james-clarke/chronicle.git"
    git fetch --unshallow --quiet "$src" HEAD 2>/dev/null || git fetch --deepen=500 --quiet "$src" HEAD 2>/dev/null \
      || echo "history: the fetch over GITHUB_TOKEN failed" >&2
  else
    git fetch --unshallow --quiet origin 2>"$tmp/fetch" || git fetch --deepen=500 --quiet origin 2>>"$tmp/fetch" \
      || echo "history: the fetch from origin failed: $(tail -1 "$tmp/fetch")" >&2
  fi
fi
echo "history: $(git rev-list --count HEAD 2>/dev/null || echo 0) commits, shallow=$(git rev-parse --is-shallow-repository 2>/dev/null || echo none)"

if git log -1 >/dev/null 2>&1; then
  rel=$(git log -1 --format=%cr | sed 's/ ago$//; s/ hours\{0,1\}/ h/; s/ minutes\{0,1\}/ min/; s/ seconds\{0,1\}/ s/')
  abs=$(TZ=UTC git log -1 --date=format-local:'%Y-%m-%d %H:%M UTC' --format=%cd)
  hash=$(git log -1 --format=%h)
  today=$(TZ=UTC git rev-list --count --since=midnight HEAD)
  week=$(git rev-list --count --since='7 days ago' HEAD)

  # the dot: quick and green within a day, steady within a week, amber after (the script re-checks on load)
  age=$(( ( $(date +%s) - $(git log -1 --format=%ct) ) / 3600 ))
  if [ "$age" -lt 24 ]; then dot=" live"; elif [ "$age" -lt 168 ]; then dot=""; else dot=" quiet"; fi

  # 30-day commit-per-day strip as one SVG path, 4 px per day, newest on the right
  TZ=UTC git log --since='30 days ago' --format=%cd --date=format-local:%Y-%m-%d | sort | uniq -c > "$tmp/days"
  path=""; max=1; i=29
  while [ "$i" -ge 0 ]; do
    d=$(date -u -d "-$i days" +%F)
    n=$(awk -v d="$d" '$2 == d { print $1 }' "$tmp/days")
    n=${n:-0}
    [ "$n" -gt "$max" ] && max=$n
    printf '%s\n' "$n" >> "$tmp/counts"
    i=$((i - 1))
  done
  x=0
  while read -r n; do
    if [ "$n" -gt 0 ]; then
      h=$(( (n * 13 + max - 1) / max )); [ "$h" -lt 2 ] && h=2
      path="${path}M$x 14v-${h}h3v${h}z"
    else
      path="${path}M$x 14v-1h3v1z"
    fi
    x=$((x + 4))
  done < "$tmp/counts"

  {
    printf '      <p class="pulse-line"><i class="pulse-dot%s"></i>last update pushed <time datetime="%s" title="%s">%s ago</time></p>\n' \
      "$dot" "$(git log -1 --format=%cI)" "$abs" "$(printf '%s' "$rel" | esc)"
    printf '      <p class="pulse-counts"><span>%s changes today · %s this week</span><svg class="pulse-strip" width="119" height="14" viewBox="0 0 119 14" aria-label="Changes per day, last 30 days"><path d="%s"/></svg></p>\n' \
      "$today" "$week" "$path"
  } > "$tmp/pulse"
  splice pulse "$tmp/pulse"
  echo "pulse: $hash, $rel ago, $age h old, $today today, $week this week"
else
  echo "pulse: no git history, placeholders kept" >&2
fi

# --- panel lines: one span per line so the stylesheet can land them one by one ----

awk '
  /<pre[^>]*><code>/ && !/--i:/ { inpre = 1; i = 0; sub(/<code>/, "<code>\001") }
  inpre {
    line = $0; head = ""; tail = ""
    if (line ~ /\001/) { head = substr(line, 1, index(line, "\001") - 1); line = substr(line, index(line, "\001") + 1) }
    if (line ~ /<\/code><\/pre>/) { tail = "</code></pre>"; sub(/<\/code><\/pre>.*/, "", line); inpre = 0 }
    cls = (line ~ /^stored /) ? " class=\"hit\"" : ""
    printf "%s<span style=\"--i:%d\"%s>%s</span>%s\n", head, i++, cls, line, tail
    next
  }
  { print }
' "$html" > "$tmp/wrapped" && cat "$tmp/wrapped" > "$html"

# --- budget -----------------------------------------------------------------

fail=0
bytes() { cat "$@" | wc -c | tr -d ' '; }
kb() { echo "$(( ($1 + 512) / 1024 )) KB"; }

third=$(grep -E '<(link|script|img|iframe|source|video|audio|object)[^>]*(src|href)="https?://' "$html" | grep -vc 'rel="canonical"' || true)
third=$((third + $(grep -cE '(url\(["'"'"']?|@import[^;]*)https?://' "$site/style.css" || true)))
js=$(awk 'BEGIN { RS = "</script>" } /<script/ { sub(/.*<script[^>]*>/, ""); n += length($0) } END { print n + 0 }' "$html")
js=$((js + $(grep -oE ' on[a-z]+="[^"]*"' "$html" | wc -c | tr -d ' ')))

grep -o '<img[^>]*>' "$html" | grep -v 'loading="lazy"' | sed -n 's/.*src="\([^"]*\)".*/\1/p' > "$tmp/fold"
sed -n '/rel="canonical"/d; s/.*<link[^>]*href="\([^"]*\)".*/\1/p' "$html" >> "$tmp/fold"
grep -o '<img[^>]*>' "$html" | sed -n 's/.*src="\([^"]*\)".*/\1/p' > "$tmp/all"
cat "$tmp/fold" >> "$tmp/all"
fold=$(( $(bytes "$html") + $(cd "$site" && sort -u "$tmp/fold" | xargs cat | wc -c) ))
total=$(( $(bytes "$html") + $(cd "$site" && sort -u "$tmp/all" | xargs cat | wc -c) ))

check() { # value limit label
  if [ "$1" -gt "$2" ]; then echo "budget: $3 = $4 over $5" >&2; fail=1; else echo "budget: $3 = $4 (limit $5)"; fi
}
check "$third" 0 "third-party requests" "$third" 0
check "$js" 2048 "inline JS" "$js B" "2 KB"
check "$fold" 256000 "above the fold" "$(kb "$fold")" "250 KB"
check "$total" 921600 "page weight" "$(kb "$total")" "900 KB"

# --- tools page: the same rules, one file (m41 chunk 5) ---------------------
tools=$site/tools.html
tthird=$(grep -E '<(link|script|img|iframe|source|video|audio|object)[^>]*(src|href)="https?://' "$tools" | grep -vc 'rel="canonical"' || true)
tjs=$(awk 'BEGIN { RS = "</script>" } /<script/ { sub(/.*<script[^>]*>/, ""); n += length($0) } END { print n + 0 }' "$tools")
ttotal=$(( $(bytes "$tools") + $(bytes "$site/style.css") + $(bytes "$site/img/mark.svg" "$site/img/favicon.svg") ))
check "$tthird" 0 "tools: third-party requests" "$tthird" 0
check "$tjs" 0 "tools: inline JS" "$tjs B" "0 B"
check "$ttotal" 153600 "tools: page weight" "$(kb "$ttotal")" "150 KB"

# --- cache busting: img/* is served immutable for a year, so every reference carries the file's hash ----
for f in "$site"/img/*.webp; do
  name=$(basename "$f")
  v=$(cksum "$f" | cut -d' ' -f1)
  sed -i.bak "s#img/$name\"#img/$name?v=$v\"#g" "$html" "$tools" && rm -f "$html.bak" "$tools.bak"
done
echo "images: $(grep -o 'img/[a-z-]*\.webp?v=[0-9]*' "$html" | sort -u | wc -l | tr -d ' ') references stamped"
exit $fail
