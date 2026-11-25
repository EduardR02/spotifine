#!/bin/bash
set -euo pipefail

engine=target/release/PlaybackEngine
# Re-sign a newly linked engine, but don't touch a previously signed resource:
# Tauri's build script watches its mtime. Apple's linker signature isn't the
# explicit ad-hoc signature used for distribution.
if signature=$(codesign --display --verbose=2 "$engine" 2>&1) &&
   [[ $signature == *'Signature=adhoc'* && $signature != *'linker-signed'* ]] &&
   codesign --verify --strict "$engine"; then
  exit 0
fi
codesign --force --sign - "$engine"
codesign --verify --strict "$engine"
