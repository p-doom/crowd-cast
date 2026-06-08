<div align="center">
  <img src="https://github.com/p-doom/crowd-code/blob/main/img/pdoom-logo.png?raw=true" width="60%" alt="p(doom)" />
</div>
<hr>
<div align="center" style="line-height: 1;">
  <a href="https://www.pdoom.org/"><img alt="Homepage"
    src="https://img.shields.io/badge/Homepage-p%28doom%29-white?logo=home&logoColor=black"/></a>
  <a href="https://huggingface.co/p-doom"><img alt="Hugging Face"
    src="https://img.shields.io/badge/%F0%9F%A4%97%20Hugging%20Face-p--doom-ffc107?color=ffc107&logoColor=white"/></a>
  <br>
  <a href="https://discord.gg/G4JNuPX2VR"><img alt="Discord"
    src="https://img.shields.io/badge/Discord-p%28doom%29-7289da?logo=discord&logoColor=white&color=7289da"/></a>
  <a href="https://github.com/p-doom"><img alt="GitHub"
    src="https://img.shields.io/badge/GitHub-p--doom-24292e?logo=github&logoColor=white"/></a>
  <a href="https://twitter.com/prob_doom"><img alt="Twitter Follow"
    src="https://img.shields.io/badge/Twitter-prob__doom-white?logo=x&logoColor=white"/></a>
  <br>
  <a href="LICENSE.md" style="margin: 2px;">
    <img alt="License" src="https://img.shields.io/badge/License-MIT-f5de53?&color=f5de53" style="display: inline-block; vertical-align: middle;"/>
  </a>
  <br>
</div>

# `crowd-cast`:  Crowd-Sourcing Months-Long Trajectories of Human Computer Work

Infrastructure for capturing paired screencast and keyboard/mouse input data.

## Quick Start

> [![Download for macOS](https://img.shields.io/badge/Download%20for%20macOS-111111?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/p-doom/crowd-cast/releases/latest/download/CrowdCast.dmg)
> [![Download for Windows](https://img.shields.io/badge/Download%20for%20Windows-0078D6?style=for-the-badge&logo=data%3Aimage%2Fsvg%2Bxml%3Bbase64%2CPHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTAgMy40NDkgOS43NSAyLjF2OS40NTFIMFptMTAuOTQ5LTEuNTAxTDI0IDB2MTEuNEgxMC45NDlaTTAgMTIuNmg5Ljc1djkuNDUxTDAgMjAuNjk5Wm0xMC45NDkgMEgyNFYyNGwtMTMuMDUxLTEuODAxWiIvPjwvc3ZnPg%3D%3D&logoColor=white)](https://github.com/p-doom/crowd-cast/releases/latest/download/crowd-cast-setup.exe)

Download the installer for your platform and run it — no admin required. On first launch crowd-cast fetches its capture components, then a quick wizard lets you choose which apps to record. From then on it lives in your system tray, records the apps you picked, and uploads in the background. Sign in with Google from the tray menu to attribute your uploads.

- **macOS** — open the `.dmg` and follow the setup wizard, then grant **Accessibility** (System Settings → Privacy & Security → Accessibility) so it can capture input.
- **Windows** — run the `.exe`; if SmartScreen shows *"Windows protected your PC"*, click **More info → Run anyway** (the installer isn't code-signed yet). No special permissions are needed.

<!-- Download links use /releases/latest/download/<asset> (stable filenames) so they never need per-release edits. Each resolves to the newest release containing that asset; while macOS and Windows ship as separate releases, only the most-recently-released platform's link resolves until releases are unified per version. -->

## Features

- **Single binary**: No external OBS installation required (libobs is embedded)
- **Privacy-aware capture**: Only records when selected applications are in the foreground
- **Automatic updates**: Sparkle (macOS) / WinSparkle (Windows) keep the app up to date in the background
- **Idle detection**: Automatically pauses recording when you step away, resumes on return
- **Hardware acceleration**: Uses native hardware encoding
- **Efficient uploads**: Streaming uploads via pre-signed S3 URLs with retry/backoff
- **Easy setup**: Wizard handles permissions and application selection

## How It Works

crowd-cast is a single-binary agent that embeds [libobs](https://github.com/obsproject/obs-studio) for screen capture and recording, eliminating the need to install OBS Studio separately.

**Key components:**

- **Embedded libobs** - Screen/window capture with hardware encoding (via [libobs-rs](https://github.com/joshprk/libobs-rs))
- **Sync Engine** - Coordinates recording with input capture, filters by frontmost app
- **Input Capture** - Cross-platform keyboard/mouse capture (rdev/evdev)
- **System Tray** - Control recording from the menu bar

```
┌─────────────────────────────────────────────────────────────────┐
│                     crowd-cast Agent (Rust)                     │
│  ┌──────────┐  ┌────────────────┐  ┌─────────────────────────┐  │
│  │ Tray UI  │  │ Embedded libobs│  │      Sync Engine        │  │
│  │          │  │ (libobs-rs)    │  │  - Frontmost app detect │  │
│  └──────────┘  │                │  │  - Input filtering      │  │
│                │  ┌───────────┐ │  │  - Event buffering      │  │
│                │  │mac-capture│ │  └───────────┬─────────────┘  │
│                │  │obs-x264   │ │              │                │
│                │  │obs-ffmpeg │ │        ┌─────┴─────┐          │
│                │  └───────────┘ │        │ rdev/evdev│          │
│                └────────────────┘        └───────────┘          │
│                        │                       │                │
│                   Video Output            Input Events          │
│                        │                       │                │
│                        └───────────┬───────────┘                │
│                                    │                            │
│                              ┌─────┴─────┐                      │
│                              │ Uploader  │                      │
│                              └─────┬─────┘                      │
└────────────────────────────────────┼────────────────────────────┘
                                     │
                                     ▼
                              ┌─────────────┐
                              │ Lambda + S3 │
                              └─────────────┘
```

## Configuration

Most settings are managed through the setup wizard and the tray menu. The configuration file is at:

- macOS: `~/Library/Application Support/dev.crowd-cast.agent/config.toml`
- Windows: `%APPDATA%\crowd-cast\agent\config\config.toml`

Key settings:

```toml
[capture]
target_apps = ["org.mozilla.firefox", "com.apple.Terminal"]
capture_all = false
idle_timeout_secs = 120          # Pause after 2 min of inactivity
single_active_app_capture = true # One app captured at a time (multi-scene)

[recording]
autostart_on_launch = true
notify_on_start_stop = true
segment_duration_secs = 300      # 5-minute recording segments

[upload]
delete_after_upload = true
```

Upload endpoint is set at build time via `CROWD_CAST_API_GATEWAY_URL`.

## Data Format

Input logs are stored in MessagePack format. Each file contains an array of `[timestamp_us, [event_type, event_data]]` tuples:

```
[0,         ["ContextChanged", ["com.apple.Terminal"]]]
[1234000,   ["KeyPress",       [0, "KeyA"]]]
[1334000,   ["KeyRelease",     [0, "KeyA"]]]
[1500000,   ["MouseMove",      [12.5, -3.2]]]
[2000000,   ["MousePress",     ["Left", 540.0, 320.0]]]
[2100000,   ["MouseRelease",   ["Left", 540.0, 320.0]]]
[2500000,   ["MouseScroll",    [0.0, -3.0, 540.0, 320.0]]]
[3999000,   ["ContextChanged", ["UNCAPTURED"]]]
```

Event types:

- `ContextChanged`: app switch (bundle ID or `UNCAPTURED` for untracked apps)
- `KeyPress` / `KeyRelease`: `[key_code, key_name]`
- `MouseMove`: `[delta_x, delta_y]`
- `MousePress` / `MouseRelease`: `[button, x, y]`
- `MouseScroll`: `[delta_x, delta_y, x, y]`

Timestamps are microseconds relative to the segment start. Video and input files share the same session/segment IDs for alignment.

## Building from Source

This section is for contributors who want to build or modify crowd-cast.

### Prerequisites

**macOS (Apple Silicon):**

```bash
brew install simde       # Required for ARM builds
brew install create-dmg  # Required for release DMG packaging
```

**Windows:** Rust (MSVC toolchain) + Visual Studio Build Tools. OBS is fetched at runtime, so no build-time OBS setup is required.

### Build & run

```bash
# Clone with submodules
git clone --recursive https://github.com/p-doom/crowd-cast.git
cd crowd-cast

# Build (endpoint required at build time; macOS auto-installs OBS binaries in build.rs)
CROWD_CAST_API_GATEWAY_URL="https://your-api-gateway.execute-api.region.amazonaws.com/prod/presign" \
  cargo build --release

# Run the setup wizard, then run normally
./target/release/crowd-cast-agent --setup
./target/release/crowd-cast-agent

# Tests
cargo test
```

On macOS, `build.rs` automatically installs OBS binaries via `cargo-obs-build` during `cargo build`. Set `CROWD_CAST_SKIP_OBS_INSTALL=1` to skip this behavior.

When run, the agent will:

1. Bootstrap OBS libraries (downloads if needed)
2. Initialize embedded libobs for capture
3. Show in your system tray
4. Capture input when selected apps are in the foreground

### Command Line Options

```
crowd-cast-agent [OPTIONS]

OPTIONS:
    -h, --help    Print help message
    -s, --setup   Run the setup wizard (re-select apps, etc.)

ENVIRONMENT:
    RUST_LOG      Set log level (e.g., debug, info, warn)
    CROWD_CAST_LOG_PATH
                  Override log directory (default: ~/Library/Logs/crowd-cast on macOS)
    CROWD_CAST_API_GATEWAY_URL
                  Lambda endpoint for pre-signed S3 URLs (set at build time)
```

### libobs-rs Integration

The agent uses [libobs-rs](https://github.com/joshprk/libobs-rs) to embed OBS functionality. Key crates:

- `libobs` - Raw FFI bindings to libobs
- `libobs-wrapper` - Safe Rust wrapper
- `libobs-bootstrapper` - Downloads OBS binaries at runtime (macOS)
- `cargo-obs-build` - Downloads OBS binaries at build time

The fork at `libobs-rs/` includes macOS support from [PR #53](https://github.com/joshprk/libobs-rs/pull/53).

### Adding New Capture Sources

To add support for new capture types, implement them in `src/capture/sources.rs`:

```rust
pub fn new_window_capture(ctx: &ObsContext, window_name: &str) -> Result<ObsSourceRef> {
    // Use libobs-wrapper to create window capture source
}
```

## Releasing

Builds are signed and published to GitHub Releases, with the auto-update appcast uploaded to S3.

### macOS

First-time setup on a release machine:

```bash
scripts/setup-macos-signing.sh \
  --p12 /path/to/developer-id.p12
```

For a full release (build, sign, notarize, publish to GitHub Releases + upload appcast to S3):

```bash
scripts/build-and-publish-macos.sh \
  --github-repo p-doom/crowd-cast \
  --s3-bucket crowd-cast-bucket \
  --identity "Developer ID Application: Your Name (TEAMID)" \
  --notarize \
  --version 1.0.0 \
  --build-number 1055 \
  --sparkle-public-ed-key "YOUR_PUBLIC_KEY" \
  --sparkle-private-ed-key-file /path/to/private-key.txt
```

Auto-updates are delivered via Sparkle using an appcast hosted on S3.

### Windows

Windows releases are cut by the GitHub Actions workflow (`.github/workflows/windows-release.yml`) — dispatch it to build, sign, and publish the installer and update its WinSparkle appcast on S3. No version bump is needed: the comparable version is `{marketing}.{run_number}`, so each run is newer than the last.

To build the per-user installer (no admin / UAC required) locally with [Inno Setup](https://jrsoftware.org/isinfo.php):

```powershell
# One-time: install the Inno Setup compiler
winget install JRSoftware.InnoSetup

# Build the release binary + installer
$env:CROWD_CAST_API_GATEWAY_URL = "https://.../prod/presign"
pwsh scripts/build-windows-installer.ps1
# -> dist/crowd-cast-setup.exe
```

The installer (`installer/windows/crowd-cast.iss`) installs the agent and its `obs.dll` loader under `%LOCALAPPDATA%\Programs\crowd-cast`, creates a Start Menu shortcut tagged with the app's AppUserModelID (so toast notifications are branded "crowd-cast"), and registers an uninstaller that stops the agent and removes the autostart entry and install directory. Autostart itself is managed in-app via the setup wizard's "start at login" option.

On first launch the agent downloads the rest of the OBS runtime (codecs, plugins) into the install folder and relaunches itself automatically — a one-time step that needs network access.

### Linux

Support coming soon...

## Backend Setup

The agent expects a Lambda endpoint that returns pre-signed S3 URLs. Example Lambda handler:

```python
import boto3
import json

s3 = boto3.client('s3')
BUCKET = 'your-bucket'

def handler(event, context):
    body = json.loads(event['body'])
    file_name = body['fileName']
    version = body['version']
    user_id = body['userId']
    
    key = f"uploads/{version}/{user_id}/{file_name}"
    
    content_type = (
        "application/msgpack" if file_name.endswith(".msgpack") else "video/mp4"
    )

    upload_url = s3.generate_presigned_url(
        'put_object',
        Params={'Bucket': BUCKET, 'Key': key, 'ContentType': content_type},
        ExpiresIn=3600
    )
    
    return {
        'statusCode': 200,
        'body': json.dumps({
            'uploadUrl': upload_url,
            'key': key,
            'contentType': content_type,
        })
    }
```

## Utilities

Overlay keylogs on top of a screen capture:

```bash
python scripts/overlay_keylogs.py --video capture.mp4 --input input.msgpack --output capture_with_keys.mp4
```

To just generate subtitles (ASS):

```bash
python scripts/overlay_keylogs.py --input input.msgpack --ass-out keylogs.ass
```

## Contributing

Contributions welcome! Please open an issue first to discuss proposed changes.

## License

MIT License, see [LICENSE.md](LICENSE.md)
