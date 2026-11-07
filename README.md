# pvz2-save-sync

A desktop app that syncs Plants vs. Zombies 2 Reflourished saves between Waydroid and an Android phone over ADB.

Two modes:

- **Manual Sync** copies the source save over the target save.
- **Automatic Sync** reads both saves and copies the newer one to the other device.

The target save is backed up locally before it is overwritten, and transfers are blocked while Reflourished is running. When the game is running, the app offers to stop it and continue.

## Requirements

- Linux with a graphical session.
- `adb` on `PATH` (Android platform-tools, or the `android-tools` / `adb` package of your distribution).
- Reflourished installed on both devices, package `com.ea.game.pvz2_rfl`.
- Waydroid reachable over ADB, usually `adb connect 192.168.240.112:5555`.
- Wireless debugging enabled on the phone. The app can pair it with a QR code.

If `adb` is missing, the app shows an error with a Retry button instead of crashing.

## Automatic Install

The preferred way to install is the curl installer. It downloads the latest release binary from GitHub.

Root install (preferred):

```sh
curl -fsSL https://raw.githubusercontent.com/xamionex/pvz2-save-sync/main/install.sh | sh
```

Installs `pvz2-save-sync` to `/usr/local/bin/pvz2-save-sync`. It re-runs itself with `sudo` when needed.

User install:

```sh
curl -fsSL https://raw.githubusercontent.com/xamionex/pvz2-save-sync/main/install.sh | sh -s -- --user
```

Installs `pvz2-save-sync` to `~/.local/bin/pvz2-save-sync`. Make sure `~/.local/bin` is on your `PATH`.

When both `/usr/local/bin/pvz2-save-sync` and `~/.local/bin/pvz2-save-sync` exist, the installer reports the other copy and asks whether to remove it, since `PATH` order decides which one runs.

## Manual Install

Requires a Rust toolchain.

```sh
cargo build --release
```

The binary is located at `target/release/pvz2-save-sync`.

## Usage

1. Start the app.
2. On **Manual Sync**, pick a device in the **Source** card and another in the **Target** card, then click **Transfer Save**.
3. On **Automatic Sync**, pick both devices and click **Sync Save**.
4. Use **Show QR code** under a device card to pair a phone through Android Wireless debugging.

The active tab and the selected devices are remembered by default. Both can be turned off under **Settings**. If a remembered device is not connected when the app starts, the pairing window opens automatically. After the phone scans the QR code, the app pairs it and selects it for that slot.

### QR pairing

1. On the phone, open **Developer options > Wireless debugging > Pair device with QR code**.
2. Click **Show QR code** in the app for the slot that should use the phone.
3. Scan the QR code displayed by the app.
4. Wait for the app to report that the device was paired and selected.

The app polls `adb mdns services` for the phone's pairing service, runs `adb pair`, and then waits for the mDNS transport to come up. When adb does not connect by itself, the app falls back to `adb connect` on the discovered address.

## Saving Behavior

Save folder on both devices:

```
/sdcard/Android/data/com.ea.game.pvz2_rfl/files/No_Backup
```

Everything at the top level of that folder is copied, except `CDN.*` and `resumableDownload`, which are device-specific caches.

A transfer:

- pulls every save entry from the source with `adb pull -a`, so modification times survive,
- backs up the target save to the local backup directory,
- removes the old save entries from the target,
- pushes the new entries,
- restores file and directory modification times,
- verifies the size and modification time of `pp.dat` on the target.

If the game is running on either device, the transfer does not start. The app offers **Stop game and continue**, which force-stops Reflourished and retries. The running state is checked again immediately before the target save is changed.

## Automatic Sync Rules

The save age is the modification time of `pp.dat`.

| Situation | Result |
| --------- | ------ |
| Left save is newer | Copied to the right device |
| Right save is newer | Copied to the left device |
| Same time and same size | Already in sync, nothing is copied |
| Same time, different size | Ambiguous, nothing is copied |
| Only one device has a save | Copied to the other device |
| Neither device has a save | Error, nothing is copied |

## Backups and Configuration

Backups are written to:

```
${XDG_DATA_HOME:-~/.local/share}/pvz2-save-sync/backups/<serial>-<timestamp>/
```

Each backup contains the complete target save as it was before the transfer, with modification times preserved.

Configuration is stored in:

```
${XDG_CONFIG_HOME:-~/.config}/pvz2-save-sync/config.json
```

## Development

```sh
cargo test
cargo clippy --all-targets
cargo build --release
```

## Releasing

Push a `v*` tag. The GitHub workflow runs the tests, builds with `cargo-zigbuild` for glibc 2.28, and publishes the raw binary as a release asset named `pvz2-save-sync`. `install.sh` downloads that asset.
