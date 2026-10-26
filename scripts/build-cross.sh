#!/usr/bin/env bash
# Cross-builds the Linux and Windows executables from macOS or Linux.
# Requires: rustup, zig, cargo-zigbuild (pip install ziglang cargo-zigbuild).
set -euo pipefail
cd "$(dirname "$0")/.."

rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-gnu >/dev/null
cargo zigbuild --release --target x86_64-pc-windows-gnu
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17

mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/pdf-cleaner.exe dist/PDFCleaner-windows-x64.exe
cp target/x86_64-unknown-linux-gnu/release/pdf-cleaner dist/PDFCleaner-linux-x64
chmod +x dist/PDFCleaner-linux-x64
tar -czf dist/PDFCleaner-linux-x64.tar.gz -C dist PDFCleaner-linux-x64

echo "Built dist/PDFCleaner-windows-x64.exe and dist/PDFCleaner-linux-x64.tar.gz"
