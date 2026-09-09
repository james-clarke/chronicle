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

if [ "$(git rev-parse --is-shallow-repository 2>/dev/null)" = true ]; then
  git fetch --unshallow --quiet 2>/dev/null || git fetch --deepen=500 --quiet 2>/dev/null || true
fi

if git log -1 >/dev/null 2>&1; then
  rel=$(git log -1 --format=%cr | sed 's/ ago$//; s/ hours\{0,1\}/ h/; s/ minutes\{0,1\}/ min/; s/ seconds\{0,1\}/ s/')
  abs=$(TZ=UTC git log -1 --date=format-local:'%Y-%m-%d %H:%M UTC' --format=%cd)
  hash=$(git log -1 --format=%h)
  subject=$(git log -1 -E --grep='^(feat|fix|perf)(\(|:|!)' --format=%s)
  today=$(TZ=UTC git rev-list --count --since=midnight HEAD)
  week=$(git rev-list --count --since='7 days ago' HEAD)

  # scope kept, milestone tag pulled out as a badge, subject cut at 80 chars
  prefix=$(printf '%s' "$subject" | sed -n 's/^\([a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}\):.*/\1/p')
  rest=$(printf '%s' "$subject" | sed 's/^[a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}: *//')
  ms=$(printf '%s' "$rest" | grep -oE '(^| )m[0-9]+' | head -1 | tr -d ' ' || true)
  [ -n "$ms" ] && rest=$(printf '%s' "$rest" | sed "s/^$ms //; s/ $ms / /")
  if [ "${#rest}" -gt 80 ]; then rest=$(printf '%s' "$rest" | cut -c1-79 | sed 's/ *$//')…; fi

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
    printf '      <p class="pulse-line"><i class="pulse-dot"></i>pushed <time datetime="%s" title="%s">%s ago</time> · <code>%s</code></p>\n' \
      "$(git log -1 --format=%cI)" "$abs" "$(printf '%s' "$rel" | esc)" "$hash"
    printf '      <p class="pulse-subject">'
    [ -n "$prefix" ] && printf '<code>%s</code> ' "$(printf '%s' "$prefix" | esc)"
    [ -n "$ms" ] && printf '<b class="pulse-ms">%s</b> ' "$ms"
    printf '%s</p>\n' "$(printf '%s' "$rest" | esc)"
    printf '      <p class="pulse-counts"><span>%s commits today · %s this week</span><svg class="pulse-strip" width="119" height="14" viewBox="0 0 119 14" aria-label="Commits per day, last 30 days"><path d="%s"/></svg></p>\n' \
      "$today" "$week" "$path"
    printf '      <ul class="pulse-recent" aria-label="Last five commits">\n'
    git log -5 --format='%h%x09%s' | while IFS="$(printf '\t')" read -r h s; do
      [ "${#s}" -gt 80 ] && s=$(printf '%s' "$s" | cut -c1-79 | sed 's/ *$//')…
      printf '        <li><code>%s</code> %s</li>\n' "$h" "$(printf '%s' "$s" | esc)"
    done
    printf '      </ul>\n'
  } > "$tmp/pulse"
  splice pulse "$tmp/pulse"
  echo "pulse: $hash, $rel ago, $today today, $week this week"

  # last ten feat|fix|perf subjects grouped by milestone tag, newest group marked in progress
  {
    printf '      <ul class="log">\n'
    group=""; first=1
    git log -10 -E --grep='^(feat|fix|perf)(\(|:|!)' --format='%cs%x09%s' | while IFS="$(printf '\t')" read -r d subj; do
      pre=$(printf '%s' "$subj" | sed -n 's/^\([a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}\):.*/\1/p')
      body=$(printf '%s' "$subj" | sed 's/^[a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}: *//')
      tag=$(printf '%s' "$body" | grep -oE '(^| )m[0-9]+' | head -1 | tr -d ' ' || true)
      [ -n "$tag" ] && body=$(printf '%s' "$body" | sed "s/^$tag //; s/ $tag / /")
      [ "${#body}" -gt 72 ] && body=$(printf '%s' "$body" | cut -c1-71 | sed 's/ *$//')…
      if [ "${tag:-none}" != "$group" ]; then
        group=${tag:-none}
        if [ "$first" = 1 ]; then
          printf '        <li class="log-ms"><b class="pulse-ms">%s</b> <em>in progress</em></li>\n' "${tag:-no milestone}"
        else
          printf '        <li class="log-ms"><b class="pulse-ms">%s</b></li>\n' "${tag:-no milestone}"
        fi
        first=0
      fi
      printf '        <li><time>%s</time><code>%s</code><span>%s</span></li>\n' "$d" "$(printf '%s' "$pre" | esc)" "$(printf '%s' "$body" | esc)"
    done
    printf '      </ul>\n'
  } > "$tmp/log"
  splice changelog "$tmp/log"
else
  echo "pulse: no git history, placeholders kept" >&2
fi

# --- panel lines: one span per line so the stylesheet can land them one by one ----

awk '
  /<pre><code>/ && !/--i:/ { inpre = 1; i = 0; sub(/<pre><code>/, "<pre><code>\001") }
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

# --- numbers from the tree (nothing that needs a cargo build) ---------------

tests=$(grep -rc '#\[test\]' crates --include='*.rs' | awk -F: '{ s += $2 } END { print s + 0 }')
crates=$(ls -d crates/*/ | wc -l | tr -d ' ')
migration=$(ls crates/core/migrations | sed 's/_.*//' | sort -n | tail -1)
edition=$(sed -n 's/^edition = "\([0-9]*\)"/\1/p' Cargo.toml | head -1)
printf '  <p class="numbers"><b>%s</b> tests · <b>%s</b> crates · migration <b>%s</b> · rust <b>%s</b> · <b>1</b> binary</p>\n' \
  "$tests" "$crates" "$migration" "$edition" > "$tmp/numbers"
splice numbers "$tmp/numbers"
echo "numbers: $tests tests, $crates crates, migration $migration, edition $edition"

# --- budget -----------------------------------------------------------------

fail=0
bytes() { cat "$@" | wc -c | tr -d ' '; }
kb() { echo "$(( ($1 + 512) / 1024 )) KB"; }

third=$(grep -E '<(link|script|img|iframe|source|video|audio|object)[^>]*(src|href)="https?://' "$html" | grep -vc 'rel="canonical"' || true)
third=$((third + $(grep -cE '(url\(["'"'"']?|@import[^;]*)https?://' "$site/style.css" || true)))
js=$(awk 'BEGIN { RS = "</script>" } /<script/ { sub(/.*<script[^>]*>/, ""); n += length($0) } END { print n + 0 }' "$html")
js=$((js + $(grep -oE ' on[a-z]+="[^"]*"' "$html" | wc -c | tr -d ' ')))
printf '  <p class="proof">This page: <b>%s</b> third-party requests, <b>%s B</b> of script, no cookies, no analytics. Counted by the build, from the file you are reading.</p>\n' "$third" "$js" > "$tmp/proof"
splice proof "$tmp/proof"

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
exit $fail
