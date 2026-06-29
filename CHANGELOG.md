# Changelog
All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project follows Semantic Versioning.

## [Unreleased]

## [1.0.5] - 2026-06-17
### Added
- Linux support for GNOME on Wayland and sway, including setup gating, native tray integration, user-space installation, bundled libobs provisioning, and signed in-app auto-updates.
- Linux release workflow for production and dev appcasts backed by GitHub Release assets and S3 feeds.

### Changed
- Migration note: when moving from 1.0.4 to 1.0.5, add `start_on_login = true` under the `[capture]` section in your config.
