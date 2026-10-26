$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo build --release
New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item "target/release/pdf-cleaner.exe" "dist/PDFCleaner-windows-x64.exe" -Force

Write-Host "Built dist/PDFCleaner-windows-x64.exe"
