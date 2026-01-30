//! Example showing how to spawn an emulator with custom JWT authentication

use android_emulator::{
    EmulatorClient, EmulatorConfig, GrpcAuthConfig, Result,
    auth::{AllowlistEntry, GrpcAllowlist},
    list_avds,
};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let avds = list_avds().await?;
    if avds.is_empty() {
        eprintln!(
            "No AVDs found. Please create an AVD using Android Studio or the avdmanager tool."
        );
        return Ok(());
    }

    let avd_name = std::env::var("AVD").unwrap_or_else(|_| avds[0].clone());
    println!("Using AVD: {}", avd_name);

    let allowlist = GrpcAllowlist {
        unprotected: vec![],
        allowlist: vec![AllowlistEntry {
            iss: "test-tool".to_string(),
            allowed: vec!["/android.emulation.control.EmulatorController/getStatus".to_string()],
            protected: vec![
                "/android.emulation.control.EmulatorController/getBattery".to_string(),
                "/android.emulation.control.EmulatorController/getGps".to_string(),
            ],
        }],
    };

    // Configure emulator with custom issuer for JWT authentication
    // This will automatically set up a gRPC allowlist
    let config = EmulatorConfig::new(avd_name.clone())
        .with_grpc_auth(GrpcAuthConfig::Jwt {
            issuer: Some("test-tool".to_string()),
        })
        .with_grpc_allowlist(allowlist)
        .with_window(false);

    println!("Starting emulator with custom JWT issuer 'test-tool'...");
    let instance = config.spawn().await?;
    println!("Emulator started at: {}", instance.grpc_endpoint());

    println!("Connecting to emulator...");
    let mut client = instance
        .connect(Some(Duration::from_secs(30)), false)
        .await?;
    println!("Connected!");

    // Test the connection
    // (by default the client can access all methods since it will lazily mint
    // and cache tokens with the appropriate 'aud' claim when needed)
    let status = client.protocol_mut().get_status(()).await?.into_inner();
    println!(
        "Emulator started: version={}, uptime={}ms, booted={}",
        status.version, status.uptime, status.booted
    );
    let battery = client.protocol_mut().get_battery(()).await?.into_inner();
    println!("Battery level: {}%", battery.charge_level);

    // Demonstrate exporting a limited token that only allows access to the getBattery method for 5 minutes
    let limited_token = client.export_token(
        &["/android.emulation.control.EmulatorController/getBattery"],
        Duration::from_secs(300),
    )?;

    // Connect limited client with the bearer token
    let provider = android_emulator::auth::AuthProvider::new_bearer(&limited_token.token);
    let mut limited_client = EmulatorClient::connect_with_auth(instance.grpc_endpoint(), provider)
        .await
        .expect("Failed to connect with bearer token");

    let result = limited_client.protocol_mut().get_gps(()).await;
    println!(
        "Result from trying to call getGps with limited token (should fail): {:?}",
        result
    );

    // Terminate the emulator
    println!("Terminating emulator...");
    instance.terminate().await?;

    Ok(())
}
