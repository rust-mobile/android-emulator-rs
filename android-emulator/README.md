# Android Emulator gRPC Control Library

A Rust library for controlling Android Emulators via gRPC.

## Features

- **Discover Emulator AVDs**: List available Android Virtual Devices
- **Spawn Emulators**: Configure and start emulator instances programmatically
- **Find Running Emulators**: Locate existing emulator instances via ADB protocol
- **gRPC Control**: Full bindings to the Android Emulator gRPC controller API
- **Authentication Support**: No auth, Basic Auth, and JWT authentication for gRPC
- **Export Limited JWT Tokens**: Export JWT tokens that can temporarily grant
  other tools / clients limited access to the emulator.

## Usage

### List Available AVDs

```rust
#[tokio::main]
async fn main() {
    let avds = android_emulator::list_avds().await.expect("Failed to list AVDs");
    println!("Available AVDs: {:?}", avds);
}
```

### Start a Headless Emulator

```rust
use android_emulator::{EmulatorConfig, list_avds};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the first available AVD
    let avds = list_avds().await?;
    let avd_name = avds.first().ok_or("No AVDs found")?;

    let config = EmulatorConfig::new(avd_name)
        .with_window(false)
        .with_snapshot_load(false)
        .with_snapshot_save(false)
        .with_boot_animation(false);

    let instance = config.spawn().await?;

    // Connect to the emulator
    let mut client = instance.connect(Some(Duration::from_secs(120)), true).await?;

    // Wait until the emulator is fully booted
    let elapsed = client.wait_until_booted(Duration::from_secs(200), None).await?;

    println!("Emulator ready at: {}", instance.grpc_endpoint());
    println!("Booted in {:.1} seconds", elapsed.as_secs_f64());
    Ok(())
}
```

### Find a Running Emulator

Find an existing emulator instance that's running a particular AVD and connect to it:

```rust,no_run
use android_emulator::{list_avds, list_emulators};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the first available AVD
    let avds = list_avds().await?;
    let avd_id = avds.first().ok_or("No AVDs found")?;

    // Find a running emulator with that AVD
    let emulators = list_emulators().await?;
    let instance = emulators
        .into_iter()
        .find(|e| e.avd_id().unwrap_or_default() == avd_id)
        .ok_or("No running emulator found")?;

    println!("Found emulator: {}", instance.serial());
    println!("gRPC endpoint: {}", instance.grpc_endpoint());

    // Connect to the emulator
    let mut client = instance.connect(Some(Duration::from_secs(30)), true).await?;

    // Use the client...
    Ok(())
}
```

### Control the Emulator via gRPC

```rust,no_run
use android_emulator::{EmulatorConfig, list_avds};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the first available AVD
    let avds = list_avds().await?;
    let avd_name = avds.first().ok_or("No AVDs found")?;

    // Start the emulator
    let instance = EmulatorConfig::new(avd_name)
        .with_window(false)
        .spawn()
        .await?;

    let mut client = instance.connect(Some(Duration::from_secs(120)), true).await?;

    // Get emulator status
    let status = client.protocol_mut().get_status(()).await?.into_inner();
    println!("Emulator status: {:?}", status);

    // Set GPS coordinates
    let gps_state = android_emulator::proto::GpsState {
        latitude: 37.7749,
        longitude: -122.4194,
        ..Default::default()
    };

    client.protocol_mut().set_gps(gps_state).await?;
    Ok(())
}
```

### Send Touch Events

```rust,no_run
use android_emulator::{EmulatorConfig, list_avds};
use android_emulator::proto::{Touch, TouchEvent};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the first available AVD
    let avds = list_avds().await?;
    let avd_name = avds.first().ok_or("No AVDs found")?;

    // Start the emulator
    let instance = EmulatorConfig::new(avd_name)
        .with_window(false)
        .spawn()
        .await?;

    let mut client = instance.connect(Some(Duration::from_secs(120)), true).await?;

    let touch = Touch {
        x: 500,
        y: 1000,
        identifier: 1,
        pressure: 100,
        ..Default::default()
    };

    let touch_event = TouchEvent {
        touches: vec![touch],
        display: 0,
    };

    client.protocol_mut().send_touch(touch_event).await?;
    Ok(())
}
```

## Android SDK

The library depends on finding an Android SDK installation in order to be able to
spawn new emulators and will look for the Android SDK as follows:

  1. `ANDROID_HOME` environment variable
  2. `ANDROID_SDK_ROOT` environment variable (deprecated)
  3. Platform-specific default locations:
     - Linux: `$HOME/Android/sdk` or `$HOME/Android/Sdk`
     - macOS: `$HOME/Library/Android/sdk`
     - Windows: `%LOCALAPPDATA%/Android/Sdk`

**Emulator AVDs**: At least one Android Virtual Device must be created


## gRPC Protocol

The library provides complete bindings to the Android Emulator Controller gRPC API, including:

- Sensor control (accelerometer, GPS, etc.)
- Physical model control (rotation, position, etc.)
- Battery state management
- Screenshot and screen streaming
- Audio streaming
- Touch, mouse, and keyboard input
- Phone calls and SMS
- VM state management
- Display configuration
- And more...

## Development

### Regenerating Protobuf Bindings

This crate includes pre-generated protobuf bindings, so consumers don't need `protoc` installed.

If you modify the `.proto` files, regenerate the bindings by running:

```bash
cargo xtask codegen
```

This will update `android-emulator/src/generated/android.emulation.control.rs`.

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/license/mit)

at your option.