# CrowdCast Installation Guide

## Requirements

- macOS 13 (Ventura) or later
- Apple Silicon or Intel

## Step 1: Download

Download `CrowdCast.dmg` from the [latest GitHub Release](https://github.com/p-doom/crowd-cast/releases/latest).

## Step 2: Install

1. Open the downloaded `CrowdCast.dmg`
2. Drag **CrowdCast.app** into `/Applications`
3. Eject the DMG

## Step 3: First launch

1. Open **CrowdCast** from `/Applications`
2. If macOS shows "CrowdCast can't be opened", go to **System Settings > Privacy & Security** and click **Open Anyway**
3. The setup wizard will ask for permissions:
  - **Accessibility** (for keyboard/mouse capture)
  - **Screen Recording** (for video capture)
  - **Notifications** (for status alerts)
   Grant all three.
4. Select which applications you want to record (e.g. VS Code, Firefox, Cursor)
5. The app restarts and begins recording automatically. Look for the CrowdCast icon in your menu bar.

> **Quarantine note:** If the app still won't open after granting permissions, run:
>
> ```bash
> xattr -cr /Applications/CrowdCast.app
> ```

## Usage

- CrowdCast runs in the task bar. Click the icon to start/stop recording, pause, or quit.
- **Settings** lets you change which apps are tracked without editing config files.
- Recording auto-starts on launch and splits into 5-minute segments.
- Segments upload automatically in the background.
- The app auto-updates silently. You'll see a notification when a new version is installed.

## Tray icon colors

- **Green**: actively recording
- **Orange**: recording active but current app is not tracked (blank video)
- **Grey**: recording stopped or paused

## Data notice

CrowdCast records screencasts and keyboard/mouse input only for the applications you select. This data is uploaded and will be published as part of an open research dataset under a Creative Commons license. You can pause or stop recording at any time from the menu bar.

## Note: Reinstalling over a previous version

If you had a previous version of CrowdCast installed and run into permission issues, run this before reinstalling:

```bash
curl https://raw.githubusercontent.com/p-doom/crowd-cast/refs/heads/delete-local-installation-script/scripts/delete-local-install.sh | sh
```

