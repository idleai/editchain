#!/usr/bin/env bash
# Run the WebdriverIO real-VS-Code harness under Xvfb and record the session
# to an MP4 via ffmpeg's x11grab.
#
# Usage:
#   ./scripts/ui-vscode-record.sh [out.mp4] [wdio-config]
#
#   out.mp4      output video path (default .ui-out/vscode-session.mp4)
#   wdio-config   wdio config to run (default ./test/vscode/wdio.conf.ts).
#                Use ./test/vscode/wdio.visual.conf.ts for the visual state
#                matrix (screenshots + animated-scroll recording).
#
# The MP4 is ALWAYS finished: ffmpeg is stopped with a graceful SIGTERM and
# waited on so it flushes the trailer even when the wdio suite fails (a
# SIGKILL fallback only fires if ffmpeg itself is stuck), and the script exits
# with the wdio suite's own exit code — not a generic failure — so CI and
# recording drivers still see the real harness result.
#
# Requires: xvfb, ffmpeg. The wdio suite must target the same DISPLAY we start.

set -euo pipefail

EXT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$EXT_ROOT/.ui-out/vscode-session.mp4}"
CONFIG="${2:-./test/vscode/wdio.conf.ts}"
DISPLAY_NUM="${DISPLAY_NUM:-99}"
RES="${RES:-1440x900}"

mkdir -p "$(dirname "$OUT")"

echo "==> Starting Xvfb on :$DISPLAY_NUM at $RES"
Xvfb ":$DISPLAY_NUM" -screen 0 "${RES}x24" -nolisten tcp &
XVFB_PID=$!

FFMPEG_PID=""
cleanup() {
  # Stop ffmpeg FIRST (before Xvfb) and wait for it to flush the MP4 trailer
  # so the video is usable even when the wdio suite failed.
  if [ -n "$FFMPEG_PID" ] && kill -0 "$FFMPEG_PID" 2>/dev/null; then
    kill "$FFMPEG_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$FFMPEG_PID" 2>/dev/null; then break; fi
      sleep 0.25
    done
    if kill -0 "$FFMPEG_PID" 2>/dev/null; then
      echo "==> ffmpeg did not exit on SIGTERM; force-stopping" >&2
      kill -KILL "$FFMPEG_PID" 2>/dev/null || true
    fi
  fi
  wait "$FFMPEG_PID" 2>/dev/null || true
  if [ -n "$OUT" ] && [ -s "$OUT" ]; then
    echo "==> Done: $OUT"
    ls -lh "$OUT" 2>/dev/null || true
  fi
  # Xvfb keeps the X server (and the ffmpeg x11grab source) alive, so it is
  # stopped after ffmpeg has flushed.
  kill "$XVFB_PID" 2>/dev/null || true
  wait "$XVFB_PID" 2>/dev/null || true
}
trap cleanup EXIT

# Wait for Xvfb to be ready.
for i in $(seq 1 20); do
  if DISPLAY=":$DISPLAY_NUM" xdpyinfo >/dev/null 2>&1; then break; fi
  sleep 0.25
done

echo "==> Recording to $OUT"
ffmpeg -y -hide_banner -loglevel error \
  -f x11grab -video_size "$RES" -framerate 15 \
  -i ":$DISPLAY_NUM" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
  "$OUT" &
FFMPEG_PID=$!

echo "==> Running wdio suite ($CONFIG) on DISPLAY=:$DISPLAY_NUM"
cd "$EXT_ROOT"
set +e
DISPLAY=":$DISPLAY_NUM" npx wdio run "$CONFIG"
WDIO_EXIT=$?
set -e
echo "==> wdio exited with status $WDIO_EXIT"

echo "==> Stopping ffmpeg (flushing MP4 trailer)"
sleep 1
exit "$WDIO_EXIT"
