#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
mkdir -p dist
cp target/release/pdf-cleaner dist/PDFCleaner-linux-x64
chmod +x dist/PDFCleaner-linux-x64
tar -czf dist/PDFCleaner-linux-x64.tar.gz -C dist PDFCleaner-linux-x64

echo "Built dist/PDFCleaner-linux-x64.tar.gz"
