use android_emulator::{Result, list_emulators};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Looking for running emulators...");
    let emulators = list_emulators().await?;

    if emulators.is_empty() {
        println!("No running emulators found");
        return Ok(());
    }

    println!("Found {} emulator(s)", emulators.len());
    for (i, instance) in emulators.iter().enumerate() {
        println!(" Emulator {i}:");
        println!("  Serial: {:?}", instance.serial());
        println!("  gRPC port: {}", instance.grpc_port());
        println!("  gRPC endpoint: {}", instance.grpc_endpoint());
        println!("  Is owned by us: {}", instance.is_owned());
        println!("  Requires JWT auth: {}", instance.requires_jwt_auth());

        // Print metadata if available
        if let Some(avd_name) = instance.avd_name() {
            println!("  AVD name: {}", avd_name);
        }
        if let Some(avd_dir) = instance.avd_dir() {
            println!("  AVD dir: {}", avd_dir);
        }
        if let Some(version) = instance.emulator_version() {
            println!("  Emulator version: {}", version);
        }
        if let Some(build) = instance.emulator_build() {
            println!("  Emulator build: {}", build);
        }
        if let Some(serial_port) = instance.port_serial() {
            println!("  Serial port: {}", serial_port);
        }
        if let Some(adb_port) = instance.port_adb() {
            println!("  ADB port: {}", adb_port);
        }

        println!("  Attempting to connect to gRPC server...");
        if let Ok(mut client) = instance.connect(Some(Duration::from_secs(5)), true).await {
            let status = client.protocol_mut().get_status(()).await?;
            let status = status.into_inner();

            println!("  Status:");
            println!("    Version: {}", status.version);
            println!(
                "    Uptime: {} ms ({:.1} seconds)",
                status.uptime,
                status.uptime as f64 / 1000.0
            );
            println!("    Booted: {}", status.booted);
            println!("    Heartbeat: {}", status.heartbeat);

            if let Some(vm_config) = &status.vm_config {
                println!("    VM Configuration:");
                let hypervisor_name = match vm_config.hypervisor_type {
                    0 => "Unknown",
                    1 => "None",
                    2 => "KVM",
                    3 => "HAXM",
                    4 => "HVF",
                    5 => "WHPX",
                    6 => "AEHD",
                    _ => "Unknown",
                };
                println!("      Hypervisor: {}", hypervisor_name);
                println!("      CPU Cores: {}", vm_config.number_of_cpu_cores);
                println!("      RAM: {} MB", vm_config.ram_size_bytes / (1024 * 1024));
            }

            if let Some(hardware_config) = &status.hardware_config
                && !hardware_config.entry.is_empty()
            {
                println!("    Hardware Configuration:");
                for entry in &hardware_config.entry {
                    println!("      {}: {}", entry.key, entry.value);
                }
            }

            if !status.guest_config.is_empty() {
                println!("    Guest Configuration:");
                let mut keys: Vec<_> = status.guest_config.keys().collect();
                keys.sort();
                for key in keys {
                    println!("      {}: {}", key, status.guest_config[key]);
                }
            }
        } else {
            println!("  Unable to connect to gRPC server");
        }
    }

    Ok(())
}
