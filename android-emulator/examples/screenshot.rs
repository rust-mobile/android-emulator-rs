//! Example: Take a screenshot from an emulator
//!
//! This example tries to connect to an existing emulator, or starts a new one if needed.
//! It then captures a screenshot and saves it to disk.
//!
//! Usage:
//!   cargo run --example screenshot

use android_emulator::proto::{ImageFormat, image_format};
use android_emulator::{EmulatorConfig, list_avds, list_emulators};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Android Emulator Screenshot Example\n");

    let emulators = list_emulators().await?;
    let mut connection = None;
    for instance in emulators.into_iter() {
        println!("Found running emulator with serial: {}", instance.serial());
        if let Ok(client) = instance
            .connect(Some(std::time::Duration::from_secs(5)), true)
            .await
        {
            println!(
                "Successfully connected to emulator with serial: {}",
                instance.serial()
            );
            connection = Some((instance, client));
            break;
        } else {
            println!(
                "Failed to connect to emulator with serial: {}, trying next one...",
                instance.serial()
            );
        }
    }

    if connection.is_none() {
        println!("No running emulators found or failed to connect. Starting a new emulator...");

        let avds = list_avds().await?;
        if avds.is_empty() {
            eprintln!(
                "No AVDs found. Please create an AVD using Android Studio or the avdmanager tool."
            );
            return Ok(());
        }

        let avd_name = std::env::var("AVD").unwrap_or_else(|_| avds[0].clone());
        println!("Using AVD: {}", avd_name);

        let config = EmulatorConfig::new(avd_name.clone()).with_window(false);
        let instance = config.spawn().await?;
        let client = instance
            .connect(Some(std::time::Duration::from_secs(10)), true)
            .await?;
        connection = Some((instance, client));
    }

    let Some((instance, mut client)) = connection else {
        eprintln!("Failed to connect to any emulator instance.");
        return Ok(());
    };

    // Wait for emulator to fully boot
    println!("\nWaiting for emulator to fully boot...");
    let elapsed = client
        .wait_until_booted(std::time::Duration::from_secs(260), None)
        .await?;
    println!("Emulator booted in {} seconds", elapsed.as_secs_f64());

    // Get emulator status
    println!("\nGetting emulator status...");
    let status = client.protocol_mut().get_status(()).await?.into_inner();
    println!("  Emulator booted: {:?}", status.booted);
    if let Some(vm_config) = status.vm_config {
        println!("  Hypervisor: {:?}", vm_config.hypervisor_type());
    }

    // Take screenshot
    println!("\nTaking screenshot...");
    let format = ImageFormat {
        format: image_format::ImgFormat::Png.into(),
        width: 0,  // Use device width
        height: 0, // Use device height
        display: 0,
        rotation: None,
        transport: None,
        folded_display: None,
        display_mode: 0,
    };

    let screenshot = client
        .protocol_mut()
        .get_screenshot(format)
        .await?
        .into_inner();
    println!("Screenshot captured!");
    println!("  Size: {} bytes", screenshot.image.len());
    println!(
        "  Width: {}",
        screenshot.format.as_ref().map(|f| f.width).unwrap_or(0)
    );
    println!(
        "  Height: {}",
        screenshot.format.as_ref().map(|f| f.height).unwrap_or(0)
    );

    // Save to file
    let filename = "screenshot.png";
    std::fs::write(filename, &screenshot.image)?;
    println!("\nScreenshot saved to: {}", filename);

    // Clean up: terminate emulator if we started it
    if instance.is_owned() {
        println!("\nTerminating emulator we started...");
        instance.terminate().await?;
    } else {
        println!("\nNot terminating emulator since we did not start it");
    }

    Ok(())
}
