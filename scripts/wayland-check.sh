#!/usr/bin/env bash
# Run a Wayland focus route against a nested compositor (m39). Neither
# route can be checked from an X11 login without one, and both Debian's
# sway and its kwin-wayland run inside an X11 window.
#
#   scripts/wayland-check.sh sway   # wlr-foreign-toplevel + ext-idle-notify
#   scripts/wayland-check.sh kwin   # the KWin script over D-Bus
#
# Prints the focus stream the daemon would record. Needs `sway` and `foot`,
# or `kwin_wayland`, `dbus-run-session` and `foot`.
set -u
route="${1:-sway}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

cargo build --quiet -p chronicle-capture --examples || exit 1
probe="$root/target/debug/examples/focus-probe"

newest_display() {
  ls -t "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | grep -v '\.lock$' | head -1
}

run_sway() {
  cat > "$work/sway.conf" <<'CONF'
output * bg #202020 solid_color
default_border none
CONF
  WLR_BACKENDS=x11 WLR_NO_HARDWARE_CURSORS=1 sway -c "$work/sway.conf" >"$work/sway.log" 2>&1 &
  local pid=$! sock=""
  for _ in $(seq 1 40); do
    sock=$(ls -t "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1)
    [ -n "$sock" ] && break
    sleep 0.25
  done
  if [ -z "$sock" ]; then
    echo "sway did not start:"; cat "$work/sway.log"; kill "$pid" 2>/dev/null; return 1
  fi
  export SWAYSOCK="$sock"
  export WAYLAND_DISPLAY="$(basename "$(newest_display)")"
  unset DISPLAY
  echo "sway on $WAYLAND_DISPLAY"
  "$probe" wlr >"$work/probe.log" 2>&1 &
  local probe_pid=$!
  sleep 1
  swaymsg exec 'foot -T window-one' >/dev/null; sleep 2
  swaymsg exec 'foot -T window-two' >/dev/null; sleep 2
  for _ in 1 2 3 4 5; do
    swaymsg focus left >/dev/null 2>&1 || swaymsg focus right >/dev/null 2>&1
    sleep 0.6
  done
  kill "$probe_pid" 2>/dev/null
  swaymsg exit >/dev/null 2>&1
  wait "$pid" 2>/dev/null
  cat "$work/probe.log"
}

run_kwin() {
  # A private session bus so a KWin that is not the user's own cannot take
  # org.kde.KWin away from a running desktop.
  HOSTDISPLAY="${DISPLAY:-:0}" WORK="$work" PROBE="$probe" \
    dbus-run-session -- "$0" --kwin-inner
}

kwin_inner() {
  export XDG_CURRENT_DESKTOP=KDE
  kwin_wayland --width 1200 --height 800 --x11-display "$HOSTDISPLAY" \
    >"$WORK/kwin.log" 2>&1 &
  local pid=$! ok=no
  for _ in $(seq 1 60); do
    if dbus-send --session --dest=org.kde.KWin --print-reply=literal \
        /Scripting org.kde.kwin.Scripting.isScriptLoaded string:probe >/dev/null 2>&1; then
      ok=yes; break
    fi
    sleep 0.5
  done
  if [ "$ok" = no ]; then
    echo "kwin_wayland did not start:"; tail -20 "$WORK/kwin.log"; kill "$pid" 2>/dev/null; return 1
  fi
  export WAYLAND_DISPLAY="$(basename "$(newest_display)")"
  unset DISPLAY
  echo "kwin on $WAYLAND_DISPLAY"
  "$PROBE" kwin >"$WORK/probe.log" 2>&1 &
  local probe_pid=$!
  sleep 2
  foot -T kwin-one >/dev/null 2>&1 & sleep 3
  foot -T kwin-two >/dev/null 2>&1 & sleep 4
  kill "$probe_pid" 2>/dev/null
  kill "$pid" 2>/dev/null
  sleep 1
  cat "$WORK/probe.log"
}

case "$route" in
  --kwin-inner) kwin_inner ;;
  sway|wlr) run_sway ;;
  kwin|kde) run_kwin ;;
  *) echo "usage: $0 [sway|kwin]" >&2; exit 2 ;;
esac
