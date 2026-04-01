# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-04-01

### Added

- `EmulatorClient::shutdown()` API to request the emulator to perform a graceful shutdown.

### Changes

- `Emulator::terminate()` renamed to `Emulator::kill()` to avoid confusion with the gRPC `Terminate` state that can be requested.

### Fixes

- Look for an 'emulator.exe' binary on Windows
- Ensure that the emulator process is spawned in a job object on Windows, so that emulator.exe and qemu processes can be killed together.

## [0.1.0] - 2026-02-09

### Added

- Initial public release of `android-emulator` crate, providing a Rust interface
  to control Android emulators via gRPC.

[unreleased]: https://github.com/rust-mobile/android-emulator-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/rust-mobile/android-emulator-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rust-mobile/android-emulator-rs/releases/tag/v0.1.0
