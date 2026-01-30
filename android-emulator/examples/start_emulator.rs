//! Example: Start an emulator and control it
//!
//! This example demonstrates starting an emulator and performing basic control operations.
//!
//! To run this example, you need:
//! 1. ANDROID_HOME environment variable set
//! 2. An AVD created (default name: "test", or set ANDROID_TEST_AVD)
//!
//! Usage:
//!   cargo run --example start_emulator
//!   ANDROID_TEST_AVD=my_avd cargo run --example start_emulator

use android_emulator::{EmulatorConfig, list_avds, proto};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let avds = list_avds().await?;
    if avds.is_empty() {
        eprintln!(
            "No AVDs found. Please create an AVD using Android Studio or the avdmanager tool."
        );
        return Ok(());
    }

    let avd_name = std::env::var("AVD").unwrap_or_else(|_| avds[0].clone());
    println!("Starting emulator with AVD: {}", avd_name);

    // Configure emulator
    let config = EmulatorConfig::new(avd_name)
        .with_window(false) // Run headless
        .with_snapshot_load(false);

    // Start emulator
    println!("Launching emulator process...");
    let instance = config.spawn().await?;
    println!("Emulator process started");
    println!("gRPC endpoint: {}", instance.grpc_endpoint());

    // Wait for gRPC server to be ready and connect
    println!("\nConnecting to gRPC server (timeout: 10 seconds)...");
    let mut client = match instance.connect(Some(Duration::from_secs(10)), true).await {
        Ok(client) => {
            println!("Connected to emulator");
            client
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            instance.terminate().await?;
            return Ok(());
        }
    };

    // Get emulator status
    println!("\nFetching emulator status...");
    let status = client.protocol_mut().get_status(()).await?.into_inner();
    println!("  version: {}", status.version);
    println!("  uptime: {} ms", status.uptime);
    println!("  booted: {:?}", status.booted);

    // Set GPS coordinates (San Francisco)
    println!("\nSetting GPS coordinates...");
    let gps_state = proto::GpsState {
        latitude: 37.7749,
        longitude: -122.4194,
        altitude: 0.0,
        speed: 0.0,
        bearing: 0.0,
        satellites: 12,
        passive_update: false,
    };
    client.protocol_mut().set_gps(gps_state).await?;
    println!(
        "GPS set to: lat={}, lon={}",
        gps_state.latitude, gps_state.longitude
    );

    // Get GPS to verify
    let gps_result = client.protocol_mut().get_gps(()).await?.into_inner();
    println!(
        "Verified GPS: lat={}, lon={}",
        gps_result.latitude, gps_result.longitude
    );

    // Simulate a touch event
    println!("\nSimulating touch event...");
    let touch = proto::Touch {
        x: 500,
        y: 1000,
        identifier: 1,
        pressure: 100,
        ..Default::default()
    };

    let touch_event = proto::TouchEvent {
        touches: vec![touch],
        display: 0,
    };

    client.protocol_mut().send_touch(touch_event).await?;
    println!("Touch event sent");

    // Release touch
    let release_touch = proto::Touch {
        x: 500,
        y: 1000,
        identifier: 1,
        pressure: 0,
        ..Default::default()
    };

    let release_event = proto::TouchEvent {
        touches: vec![release_touch],
        display: 0,
    };

    client.protocol_mut().send_touch(release_event).await?;
    println!("Touch released");

    println!("\nWaiting for emulator to fully boot...");
    let elapsed = client
        .wait_until_booted(std::time::Duration::from_secs(260), None)
        .await?;
    println!("\nEmulator booted in {} seconds", elapsed.as_secs_f64());

    let status = client.protocol_mut().get_status(()).await?.into_inner();
    println!("  uptime: {} ms", status.uptime);
    println!("  booted: {:?}", status.booted);

    println!("\nTerminating emulator...");
    instance.terminate().await?;

    Ok(())
}
