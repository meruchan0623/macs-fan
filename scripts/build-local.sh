#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
npm ci
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
