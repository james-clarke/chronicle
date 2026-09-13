#!/bin/sh
# The site's screenshots, from the fixtures cast.
#
#   scripts/site-shots.sh seed    # replay fixtures/site/*.jsonl into a sandbox
#   scripts/site-shots.sh shots   # capture every view the page references
#   scripts/site-shots.sh         # both
#
# The sandbox is a data dir under target/ (never the live one); the UI is
# launched against it with the env hooks the visual-test loop already has
# (`CHRONICLE_UI_VIEW`, `CHRONICLE_UI_TASK`, `CHRONICLE_UI_SETTINGS`) and
# captured with ImageMagick's `import`. X11 only, and the pointer must stay
# out of the window while it runs. Two runs of the same view differ only by
# the clock and by whatever the local model wrote.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
SB=${SB:-$root/target/site-sandbox}
BIN=${BIN:-$root/target/release/chronicle}
OUT=${OUT:-$root/site/img/src}
MODELS=${MODELS:-$HOME/.local/share/chronicle/models}
export XDG_DATA_HOME=$SB
data=$SB/chronicle
db=$data/chronicle.db
step=${1:-all}

sql() { python3 -c 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit()' "$db" "$1"; }
sqlq() { python3 -c 'import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print("\n".join(str(r[0]) for r in c.execute(sys.argv[2])))' "$db" "$1"; }

# --- seed -------------------------------------------------------------------

seed() {
  command rm -rf "$data"
  mkdir -p "$data"
  command cp -f "$root/fixtures/site/config.toml" "$data/config.toml"
  [ -d "$MODELS" ] && ln -sfn "$MODELS" "$data/models"

  # Sam's declared tasks, before the first placement so the segmenter has
  # evidence to place against. Order matters: the newest declared task of a
  # project is its sink (m35), so the week's main work is declared last in
  # each project. The description is the "next step" Home shows.
  "$BIN" task add "ACME-11374 staging deploys from a tag" --project contoso \
    --description "Move staging deploys off the branch push and onto a signed tag so a deploy is a decision. Workflow written; next is the GitHub environment protection rule."
  "$BIN" task add "ACME-11381 retry backoff on SMS sends" --project mailer \
    --description "Exponential backoff with jitter on Twilio 429s and 5xx, capped at five tries. PR #412 is up; next is the review round and a canary on northwind-qa."
  "$BIN" task add "ACME-11390 northwind renewal batch times out" --project northwind \
    --description "The nightly renewal batch on northwind-memberships hits the 30 s dyno timeout past 40k members. Chunked it in PR #418; next is confirming the QA run finishes under ten minutes."
  "$BIN" task add "ACME-11382 membership renewal reminder emails" --project contoso \
    --description "Reminder emails at 30, 7 and 1 days before a membership lapses, from the nightly renewals task. Templates and tests are in; next is the review on PR #415 and the copy from the design review."

  # The week, one fixture per day, placed by the segmenter tier the shipped
  # config uses (`derive_mode = "segmenter"`), batch by batch as the daemon
  # would have. The batch ids land in batches.txt for a look afterwards.
  cases=""
  for pair in mon=2026-09-07 tue=2026-09-08 wed=2026-09-09 thu=2026-09-10 fri=2026-09-11; do
    cases="$cases --case $root/fixtures/site/${pair%%=*}.jsonl=${pair#*=}"
  done
  # shellcheck disable=SC2086
  "$BIN" replay $cases --reconcile > "$SB/batches.txt"

  # The corrections a person makes on a Thursday afternoon: two of the
  # model's names replaced, a meeting named, one minted task thrown out so
  # its time is loose again (what the organize view is for).
  by() { sqlq "select id from tasks where label like '$1%' limit 1"; }
  "$BIN" task rename "$(by 'Team standup')" --label "standup and #platform-eng"
  "$BIN" task rename "$(by 'Reviewing renewal email design')" --label "renewal emails design review" --project contoso
  "$BIN" task rename "$(by 'Participating in a video meeting')" --label "1:1 with Jordan"
  loose=$(by 'Researching cron job')
  [ -n "$loose" ] && sql "DELETE FROM intervals WHERE task_id=$loose; UPDATE tasks SET status='closed', closed_ts=$(date +%s)000, closed_by='user' WHERE id=$loose;"

  # The morning intent (m26), so Home opens on the task list rather than
  # the "Today I'm on…" picker.
  sql "INSERT OR REPLACE INTO meta (key, value) VALUES ('intent:$(date +%F)', '{\"task_ids\":[$(by 'ACME-11382'),$(by 'ACME-11390')],\"text\":\"PR #415 review round, then the northwind-qa run\"}');"

  # Settings › Model: a cloud backend on the writing routes, with the row a
  # click on "test" writes. Written after the replay so nothing above ran
  # against the placeholder key.
  cat > "$data/models.toml" <<'EOF'
max_usd_per_day = 2.0

[backends.anthropic]
kind = "anthropic"
model = "claude-sonnet-5"
api_key = "sk-ant-api03-demo-key-not-real-0000000000000000"

[routes]
chat = "anthropic"
narrative = "anthropic"
standup = "anthropic"
journal = "anthropic"
task_description = "anthropic"
checkpoint = "anthropic"
suggest_task = "anthropic"
name_task = "anthropic"
advise = "anthropic"
EOF
  sql "INSERT OR REPLACE INTO meta (key, value) VALUES ('cloud_probe:anthropic', '{\"ok\":true,\"ms\":1180,\"error\":null,\"ts\":$(date +%s)000}');"
  echo "seeded: $(sqlq 'select count(*) from spans') spans, $(sqlq 'select count(*) from intervals') intervals, $(sqlq 'select count(*) from tasks') tasks"
}

# --- shots ------------------------------------------------------------------

# meta rows the UI reads at boot: size in logical points, zoom 1.0, no
# remembered position (a stale one parks the window off-screen at scale 1.6).
window() {
  sql "INSERT OR REPLACE INTO meta (key, value) VALUES ('ui_window_size', '$1'); INSERT OR REPLACE INTO meta (key, value) VALUES ('ui_zoom_factor', '1.0'); DELETE FROM meta WHERE key='ui_window_pos';"
}

# shot NAME SIZE [ENV=VALUE ...]: launch the UI, wait for its window, capture.
shot() {
  name=$1; size=$2; shift 2
  window "$size"
  env "$@" WINIT_X11_SCALE_FACTOR="${SCALE:-1.6}" "$BIN" ui >/dev/null 2>&1 &
  pid=$!
  w=""
  for _ in $(seq 1 40); do
    sleep 0.25
    w=$(wmctrl -lp | awk -v p="$pid" '$3==p{print $1; exit}')
    [ -n "$w" ] && break
  done
  if [ -z "$w" ]; then
    echo "$name: no window for pid $pid" >&2; kill "$pid" 2>/dev/null || true; return 1
  fi
  wmctrl -i -r "$w" -b add,above
  xdotool windowraise "$w"
  sleep "${SETTLE:-3}"
  ok=""
  for _ in 1 2 3 4; do
    if import -window "$w" "$OUT/$name.png" 2>/dev/null; then ok=1; break; fi
    sleep 0.5
  done
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  [ -n "$ok" ] && echo "$name: $(magick identify -format '%wx%h' "$OUT/$name.png")"
}

shots() {
  mkdir -p "$OUT"
  task=$(sqlq "select id from tasks where label like 'ACME-11382%' limit 1")
  shot home-wide 900,700
  # The wipe shows a whole day, so the timeline opens on Thursday.
  shot timeline-wide 900,700 CHRONICLE_UI_VIEW=timeline CHRONICLE_UI_DAY=2026-09-10 CHRONICLE_UI_TASK="$task"
  shot reports-wide 900,700 CHRONICLE_UI_VIEW=reports
  shot triage 400,640 CHRONICLE_UI_VIEW=triage
  # Settings is tall; a lower scale fits the Model section on a 1080 px screen.
  SCALE=1.2 shot settings-model 900,880 CHRONICLE_UI_VIEW=settings CHRONICLE_UI_SETTINGS=Model
  shot home 400,640
}

# --- the files the page references --------------------------------------------

# og.png: the link-preview card, 1200x630 — the macOS desktop art the hero
# already ships, the wide Home capture over it, and the hero's own words.
og() {
  fonts=$root/crates/app/assets/fonts
  magick "$root/site/img/desk-mac.webp" -resize 1200x675^ -gravity center -extent 1200x630 \
    -modulate 55,85 \
    \( "$OUT/home-wide.png" -resize 640x \) -gravity northwest -geometry +530+90 -composite \
    -fill white -font "$fonts/Inter-Medium.ttf" -pointsize 68 -interline-spacing 2 \
    -annotate +62+92 "Your day,\nwritten down." \
    -fill "#e6e6e6" -font "$fonts/Inter-Regular.ttf" -pointsize 27 -interline-spacing 6 \
    -annotate +64+300 "A small window that knows what\nyou worked on, for how long,\nand why. On your own disk." \
    -fill "#bdbdbd" -font DejaVu-Sans-Mono -pointsize 22 \
    -annotate +64+556 "chronicle · local first, open source" \
    "$OUT/og.png"
  echo "og: $(magick identify -format '%wx%h' "$OUT/og.png")"
}

# webp: every capture the page references, cropped where the page shows a
# detail rather than the whole window. Offsets are in captured pixels
# (scale 1.6); adjust them when the Settings form moves.
webp() {
  q=85
  for n in home-wide timeline-wide reports-wide og; do
    magick "$OUT/$n.png" -quality $q "$root/site/img/$n.webp"
  done
  magick "$OUT/triage.png" -crop 678x470+0+0 +repage -quality $q "$root/site/img/triage.webp"
  magick "$OUT/settings-model.png" -crop "${MODEL_CROP:-880x800+300+275}" +repage -quality $q "$root/site/img/model.webp"
  ls -l "$root"/site/img/*.webp | awk '{print $5, $9}'
}

case $step in
  seed) seed ;;
  shots) shots ;;
  og) og ;;
  webp) webp ;;
  all) seed; shots; og; webp ;;
  *) echo "usage: $0 [seed|shots|og|webp|all]" >&2; exit 2 ;;
esac
