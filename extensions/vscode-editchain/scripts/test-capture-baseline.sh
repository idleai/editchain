#!/usr/bin/env bash
set -euo pipefail

# VS Code 1.85 uses Chromium 114, whose driver predates Chrome for Testing.
# WDIO's current automatic downloader uses the newer archive and receives 404.
# https://developer.chrome.com/docs/chromedriver/downloads/version-selection
capture_extension_root="$(cd "$(dirname "$0")/.." && pwd)"
capture_driver_dir="$capture_extension_root/.wdio-vscode-service/chromedriver-114.0.5735.90"
if [[ ! -x "$capture_driver_dir/chromedriver" ]]; then
    mkdir -p "$capture_driver_dir"
    curl --fail --location --retry 2 \
        https://chromedriver.storage.googleapis.com/114.0.5735.90/chromedriver_linux64.zip \
        -o "$capture_driver_dir/driver.zip"
    unzip -o -q "$capture_driver_dir/driver.zip" -d "$capture_driver_dir"
fi

cd "$capture_extension_root"
EDITCHAIN_CAPTURE_VSCODE=1.85.0 CHROMEDRIVER_PATH="$capture_driver_dir/chromedriver" \
    npm run ui:vscode:capture
