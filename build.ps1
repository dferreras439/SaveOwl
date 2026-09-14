$ErrorActionPreference = "Stop"
cargo build --release
Write-Host "Built: target\release\saveowl.exe"
