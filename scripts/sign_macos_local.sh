#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此脚本只支持 macOS。" >&2
  exit 1
fi

bundle="${1:-target/release/bundle/macos/CodexFlow.app}"
if [[ ! -d "$bundle" ]]; then
  echo "找不到应用包：$bundle" >&2
  exit 1
fi

identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle/Contents/Info.plist")"
codesign --force --sign - --identifier "$identifier" --timestamp=none "$bundle"
codesign --verify --verbose=2 "$bundle"
echo "本机应用包已签名并验证：$bundle ($identifier)"
