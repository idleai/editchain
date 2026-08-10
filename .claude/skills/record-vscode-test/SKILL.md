---
name: record-vscode-test
description: Record the EditChain VS Code extension test session as video. Use this when the user asks to "record the vscode test", "capture the vscode session", "record a video of the test", "record the ui:vscode run", or wants a video artifact of the WebdriverIO real-VS-Code harness run. Runs the wdio suite under Xvfb and captures the display with ffmpeg to an MP4.
disable-model-invocation: false
---

Record the EditChain VS Code extension's WebdriverIO test session as an MP4 video.

The extension lives at `extensions/vscode-editchain/`. The recording wrapper is
`extensions/vscode-editchain/scripts/ui-vscode-record.sh`. It starts Xvfb on a
fixed display, captures it with ffmpeg (`x11grab`), runs the wdio suite against
that display, then stops ffmpeg and saves the MP4.

## Prerequisites

- `xvfb` and `ffmpeg` installed (both are present in this environment).
- The Rust service binary built: `cargo build -p editchain-vscode-service`.
- The wdio suite deps installed: `npm install` in `extensions/vscode-editchain/`.

## Usage

Run from the extension directory:

```sh
cd extensions/vscode-editchain
./scripts/ui-vscode-record.sh [out.mp4]
```

- Default output: `.ui-out/vscode-session.mp4`.
- Pass a path to override the output location.
- Env overrides: `DISPLAY_NUM` (default `99`), `RES` (default `1440x900`).

## What it does

1. Starts `Xvfb :99 -screen 0 1440x900x24`.
2. Starts `ffmpeg -f x11grab -video_size 1440x900 -framerate 15 -i :99` → h264 MP4.
3. Runs `npx wdio run ./test/vscode/wdio.conf.ts` on `DISPLAY=:99`.
4. Stops ffmpeg, saves the MP4.

## The test suite

`test/vscode/history.e2e.ts` runs three tests:
1. Loads VS Code and asserts the workbench title contains `editchain`.
2. Opens the history explorer webview, injects the text-only layout probe
   (`test/harness/layoutProbe.js`), and runs textual layout checks.
3. Scrolls through the full history (953 rows with submodules hidden) and
   verifies all rows render down to the genesis ChainStart op. The scroll is
   **animated** (`smoothScrollTo` eases `scrollTop` over ~1.5s per pass) so the
   rows visibly flow past in the recording, rather than jumping instantly.

## After recording

Report to the user:
- The output video path and its size/duration (verify with `ffprobe`).
- The test results (PASSED/FAILED, row counts, layout check pass/fail counts).
- Any layout checks that failed (e.g. `DOT_ROW_ALIGNMENT`) — these are genuine
  findings for the agent to fix, not test blockers.

## Notes

- The `.ui-out/` directory is gitignored; videos there are not committed.
- The `.wdio-vscode-service/` cache (downloaded VS Code, ~1GB) and `trace/`
  output are gitignored.
- If a run fails with "Missing X server or $DISPLAY", ensure xvfb is installed
  and use the wrapper (it starts Xvfb itself).