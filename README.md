# SaveOwl 0.1.1

SaveOwl is a Windows tray application that tries to provide Steam-Cloud-like save handling for arbitrary games.

## What this MVP does

- Lives entirely in the Windows notification area (tray).
- Uses Windows PDH `GPU Engine` counters to identify sustained GPU-heavy processes.
- Starts a session after the same process stays above the configurable threshold for several samples.
- Uses ETW's `Microsoft-Windows-Kernel-File` provider to associate file I/O with a PID and records files mutated by the detected game's process tree.
- Filters obvious caches/logs/temp data and only considers user-profile paths.
- On session end, creates immutable, content-addressed snapshots with BLAKE3 blobs.
- Tracks snapshot parentage rather than relying on timestamps.
- Can use any ordinary folder as an upstream transport. Point that folder at Syncthing, OneDrive, Dropbox, etc.
- Detects divergence from the last upstream head. A native Windows prompt offers:
  - **Yes**: keep/publish local
  - **No**: restore upstream
  - **Cancel**: keep both branches and overwrite neither
- Never deletes the losing branch merely because a conflict was detected.

The ETW path may require administrator rights depending on machine policy. SaveOwl intentionally does **not** install a filesystem minifilter driver.

## Build on Windows

Install Rust stable and Visual Studio Build Tools with the Desktop C++ workload, then:

```powershell
cargo build --release
```

The output will be `target\release\saveowl.exe`.

## Configuration

On first run SaveOwl writes:

`%APPDATA%\SaveOwl\config.toml`

Example:

```toml
gpu_threshold_percent = 25.0
activation_samples = 3
end_samples = 5
poll_ms = 1000
max_snapshot_file_mb = 512
upstream_dir = "D:\\Syncthing\\SaveOwlCloud"
ignore_processes = ["dwm.exe", "explorer.exe", "obs64.exe", "chrome.exe", "msedge.exe", "firefox.exe"]
```

Set `upstream_dir` to a directory synchronized by the transport of your choice. The directory is treated as a dumb object store; SaveOwl owns ancestry/conflict semantics.

## Data model

Local application data lives under `%LOCALAPPDATA%\SaveOwl`:

- `blobs/<prefix>/<blake3>` — content-addressed file bodies
- `games/<game-key>/snapshots/<uuid>.json` — immutable manifests
- `games/<game-key>/state.json` — selected local head and last-seen upstream head
- `games/<game-key>/conflicts/` — preserved divergent heads

A game's key is a short BLAKE3 hash of its executable path.

## Known gaps / next engineering steps

1. Better ETW rename/delete handling and validation across Windows 10/11 builds.
2. Persist discovered save manifests per game, so subsequent sessions can restore before launch even before GPU detection fires.
3. Add a launch gate (Playnite integration would be ideal) so upstream reconciliation happens *before* game code reads the save.
4. Replace the three-way MessageBox with a proper conflict window showing timestamps, file diffs and snapshot ancestry.
5. Add retention/garbage collection for unreachable blobs.
6. Add signing, installer, autostart, structured logs and crash reports.
7. Optionally ingest Ludusavi manifests as high-confidence seed paths.

## Why ETW rather than a filesystem watcher?

A normal directory watcher tells you that a path changed but not which process caused it. The ETW event record includes process identity; SaveOwl uses that to limit candidate save files to the detected process tree. Windows documents file I/O ETW events including create/write operations, and FerrisETW provides a Rust consumer abstraction.

## License

MIT.
