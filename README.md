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

## Overview

crowd-cast is a single-binary agent that embeds [libobs](https://github.com/obsproject/obs-studio) for screen capture and recording, eliminating the need to install OBS Studio separately.

**Key components:**

- **Embedded libobs** - Screen/window capture with hardware encoding (via [libobs-rs](https://github.com/joshprk/libobs-rs))
- **Sync Engine** - Coordinates recording with input capture, filters by frontmost app
- **Input Capture** - Cross-platform keyboard/mouse capture (rdev/evdev)
- **System Tray** - Control recording from the menu bar

## Architecture

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

## Features

- **Single binary**: No external OBS installation required (libobs is embedded)
- **Privacy-aware capture**: Only records when selected applications are in the foreground
- **Automatic updates**: Sparkle framework keeps the app up to date in the background
- **Idle detection**: Automatically pauses recording when you step away, resumes on return
- **Hardware acceleration**: Uses native encoding (VideoToolbox on macOS)
- **Efficient uploads**: Streaming uploads via pre-signed S3 URLs with retry/backoff
- **Easy setup**: Wizard handles permissions and application selection

## Quick Start

### For users

Download `CrowdCast.dmg` from the [latest release](https://github.com/p-doom/crowd-cast/releases/latest), open it and follow the instructions in the wizard. 

### Building from source

```bash
# Clone the repository
git clone https://github.com/p-doom/crowd-cast.git
cd crowd-cast

# Build (endpoint required at build time)
CROWD_CAST_API_GATEWAY_URL="https://your-api-gateway.execute-api.region.amazonaws.com/prod/presign" \
  cargo build --release

# Run the setup wizard
./target/release/crowd-cast-agent --setup
```

On macOS, `build.rs` automatically installs OBS binaries via `cargo-obs-build` during
`cargo build`. Set `CROWD_CAST_SKIP_OBS_INSTALL=1` to skip this behavior.

## Platform-Specific Setup

### macOS

1. Grant **Accessibility** permission to the agent (System Settings → Privacy & Security → Accessibility)

#### macOS Distribution

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

### Linux/Windows

Support coming soon...

## Configuration

Most settings are managed through the setup wizard and the tray menu. The configuration file is at:

- macOS: `~/Library/Application Support/dev.crowd-cast.agent/config.toml`

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

## Utilities

Overlay keylogs on top of a screen capture:

```bash
python scripts/overlay_keylogs.py --video capture.mp4 --input input.msgpack --output capture_with_keys.mp4
```

To just generate subtitles (ASS):

```bash
python scripts/overlay_keylogs.py --input input.msgpack --ass-out keylogs.ass
```

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

## Development

This section is for contributors who want to modify crowd-cast.

### First Run (Recommended)

```bash
crowd-cast-agent --setup
```

This runs the interactive setup wizard that guides you through configuration.

### Normal Usage

```bash
crowd-cast-agent
```

The agent will:

1. Bootstrap OBS libraries (downloads if needed)
2. Initialize embedded libobs for capture
3. Show in your system tray
4. Capture input when selected apps are in foreground

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

### Building from Source

#### Prerequisites

**macOS (Apple Silicon):**

```bash
brew install simde       # Required for ARM builds
brew install create-dmg  # Required for release DMG packaging
```

#### Build Steps

```bash
# 1. Clone with submodules
git clone --recursive https://github.com/p-doom/crowd-cast.git
cd crowd-cast

# 2. Build the agent (macOS auto-installs OBS binaries in build.rs)
cargo build

# 3. Run tests
cargo test
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

## License

MIT License, see [LICENSE.md](LICENSE.md)

## Contributing

Contributions welcome! Please open an issue first to discuss proposed changes.