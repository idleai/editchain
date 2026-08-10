#!/usr/bin/env bash
# Run the WebdriverIO real-VS-Code harness under Xvfb and record the session
# to an MP4 via ffmpeg's x11grab.
#
# Usage:
#   ./scripts/ui-vscode-record.sh [out.mp4]
#
# Requires: xvfb, ffmpeg. The wdio suite must target the same DISPLAY we start.

set -euo pipefail

EXT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$EXT_ROOT/.ui-out/vscode-session.mp4}"
DISPLAY_NUM="${DISPLAY_NUM:-99}"
RES="${RES:-1440x900}"

mkdir -p "$(dirname "$OUT")"

echo "==> Starting Xvfb on :$DISPLAY_NUM at $RES"
Xvfb ":$DISPLAY_NUM" -screen 0 "${RES}x24" -nolisten tcp &
XVFB_PID=$!
trap 'kill $XVFB_PID 2>/dev/null || true' EXIT

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
trap 'kill $FFMPEG_PID $XVFB_PID 2>/dev/null || true' EXIT

echo "==> Running wdio suite on DISPLAY=:$DISPLAY_NUM"
cd "$EXT_ROOT"
DISPLAY=":$DISPLAY_NUM" npx wdio run ./test/vscode/wdio.conf.ts

echo "==> Stopping ffmpeg"
sleep 1
kill $FFMPEG_PID 2>/dev/null || true
wait $FFMPEG_PID 2>/dev/null || true

echo "==> Done: $OUT"
ls -lh "$OUT"