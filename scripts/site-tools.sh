#!/bin/sh
# Splice the registry's tools table into site/tools.html (m41 chunk 5):
# `chronicle connections --html` between the <!-- tools --> markers. Run
# after any registry edit; the drift test in crates/core/tests fails until
# you do. Uses the debug build when it is newer than the installed binary.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/debug/chronicle
[ -x "$bin" ] || bin=chronicle
html=$root/site/tools.html
tmp=${TMPDIR:-/tmp}/site-tools.$$
trap 'rm -f "$tmp" "$tmp.html"' EXIT
"$bin" connections --html > "$tmp"
awk -v src="$tmp" '
  /<!-- tools -->/ { print; while ((getline line < src) > 0) print line; close(src); skip = 1; next }
  /<!-- \/tools -->/ { skip = 0 }
  !skip { print }
' "$html" > "$tmp.html" && cat "$tmp.html" > "$html"
echo "site/tools.html: $(grep -c '<tr id=' "$html") rows"
