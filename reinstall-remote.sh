#!/usr/bin/env bash
# Compatibility entry point; the installer now supports desktop and remote VS Code.
set -euo pipefail
editchain_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$editchain_root/reinstall-vscode.sh" "$@"
