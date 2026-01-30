//! Android Emulator gRPC Control Tests

use android_emulator::{
    EmulatorClient, EmulatorConfig, EmulatorError, GrpcAuthConfig, get_android_home, list_avds,
    proto,
};
use serial_test::serial;
use std::env;
use std::panic;
use std::sync::Arc;
use std::time::Duration;

/// Helper to get test AVD name from environment or use default
fn get_test_avd() -> String {
    env::var("ANDROID_TEST_AVD").unwrap_or_else(|_| "test".to_string())
}

/// Helper to check if test AVD exists, panic with helpful message if not
async fn ensure_test_avd_exists() {
    if get_android_home().await.is_err() {
        panic!(
            "ANDROID_HOME environment variable not set. Please set it to your Android SDK path."
        );
    }

    let avd_name = get_test_avd();
    let avds = list_avds().await.expect("Failed to list AVDs");

    if !avds.contains(&avd_name) {
        panic!(
            "AVD '{}' not found. Available AVDs: {:?}\n\n\
            To create a test AVD, run:\n\
            avdmanager create avd -n {} -k 'system-images;android-36;google_apis_playstore;x86_64'\n\n\
            Or set ANDROID_TEST_AVD to an existing AVD name.",
            avd_name, avds, avd_name
        );
    }
}

/// Helper to spawn an emulator with a custom config, run a test closure, and handle cleanup
/// If the test panics, the emulator log contents are printed before resuming the panic.
async fn spawn_test_emulator_with_config<F, Fut>(
    mut config: EmulatorConfig,
    allow_basic_auth: bool,
    test_fn: F,
) where
    F: FnOnce(EmulatorClient, Arc<android_emulator::Emulator>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    ensure_test_avd_exists().await;

    println!("Starting emulator with custom config");

    // Configure output redirection and window settings
    config = config
        .with_window(false)
        .with_snapshot_load(false)
        .with_snapshot_save(false)
        .with_boot_animation(false);

    let instance = config.spawn().await.expect("Failed to start emulator");
    let instance = Arc::new(instance);
    println!("Emulator started, waiting for gRPC server...");

    // Connect to emulator with 30 second timeout
    let client = match instance
        .connect(Some(Duration::from_secs(30)), allow_basic_auth)
        .await
    {
        Ok(client) => {
            println!("gRPC server is ready");
            client
        }
        Err(e) => {
            instance.terminate().await.ok();
            panic!("Failed to connect to gRPC server: {}", e);
        }
    };

    // Spawn the test as a task so we can catch panics
    let instance_clone = instance.clone();
    let test_handle = tokio::spawn(async move { test_fn(client, instance_clone).await });

    // Wait for the test to complete
    let test_result = test_handle.await;

    // Always terminate the emulator
    println!("Terminating emulator...");
    instance.terminate().await.ok();

    // If the test panicked, dump the log file
    if let Err(join_error) = test_result {
        /*
        if join_error.is_panic() {
            println!("\n========== Emulator Log (from {}) ==========", log_file);
            match tokio::fs::read_to_string(&log_file).await {
                Ok(contents) => {
                    println!("{}", contents);
                }
                Err(e) => {
                    eprintln!("Failed to read emulator log file: {}", e);
                }
            }
            println!("========== End Emulator Log ==========\n");
        }
        */

        // Resume the panic
        if join_error.is_panic() {
            panic::resume_unwind(join_error.into_panic());
        } else {
            // Task was cancelled, just report it
            panic!("Test task was cancelled: {}", join_error);
        }
    }
}

/// Helper to spawn an emulator with default settings, run a test closure, and handle cleanup
/// If the test panics, the emulator log contents are printed before resuming the panic.
async fn spawn_test_emulator<F, Fut>(test_fn: F)
where
    F: FnOnce(EmulatorClient, Arc<android_emulator::Emulator>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let avd_name = get_test_avd();
    let config = EmulatorConfig::new(avd_name)
        .with_grpc_auth(GrpcAuthConfig::None)
        .with_grpc_port(8554);

    spawn_test_emulator_with_config(config, true, test_fn).await;
}

/// Test that we can list available AVDs
#[tokio::test]
#[test_log::test]
async fn test_list_avds() {
    match get_android_home().await {
        Ok(_) => {
            let avds = list_avds().await;
            match avds {
                Ok(avd_list) => {
                    println!("Found {} AVDs: {:?}", avd_list.len(), avd_list);
                    assert!(!avd_list.is_empty(), "Expected at least one AVD");
                }
                Err(EmulatorError::NoAvdsFound) => {
                    panic!(
                        "No AVDs found. Create one with:\n\
                        avdmanager create avd -n test -k 'system-images;android-36;google_apis_playstore;x86_64'"
                    );
                }
                Err(e) => panic!("Failed to list AVDs: {}", e),
            }
        }
        Err(_) => {
            println!("Skipping test: ANDROID_HOME not set");
        }
    }
}

/// Test starting an emulator and connecting via gRPC
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_start_emulator_and_connect() {
    spawn_test_emulator(|mut client, _emulator| async move {
        println!("Connected to emulator, getting status...");
        let status = client
            .protocol_mut()
            .get_status(())
            .await
            .expect("Failed to get status")
            .into_inner();

        println!("Emulator status: {:?}", status);
        println!("Test completed successfully");
    })
    .await;
}

/// Test connecting to an emulator and checking status
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_connect_to_running_emulator() {
    spawn_test_emulator(|mut client, _emulator| async move {
        println!("Connected to running emulator");

        let status = client
            .protocol_mut()
            .get_status(())
            .await
            .expect("Failed to get status")
            .into_inner();

        println!("Emulator status: {:?}", status);

        // Try to get battery status
        let battery = client
            .protocol_mut()
            .get_battery(())
            .await
            .expect("Failed to get battery")
            .into_inner();

        println!("Battery status: {:?}", battery);
    })
    .await;
}

/// Test getting and setting GPS coordinates
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_gps_control() {
    spawn_test_emulator(|mut client, _emulator| async move {
        println!("Testing GPS control");

        // Set GPS coordinates (San Francisco)
        let gps_state = proto::GpsState {
            latitude: 37.7749,
            longitude: -122.4194,
            ..Default::default()
        };

        client
            .protocol_mut()
            .set_gps(gps_state)
            .await
            .expect("Failed to set GPS");

        println!(
            "Set GPS to: lat={}, lon={}",
            gps_state.latitude, gps_state.longitude
        );

        // Get GPS coordinates
        let result = client
            .protocol_mut()
            .get_gps(())
            .await
            .expect("Failed to get GPS")
            .into_inner();

        println!("Got GPS: lat={}, lon={}", result.latitude, result.longitude);
    })
    .await;
}

/// Test getting VM state
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_vm_state() {
    spawn_test_emulator(|mut client, _emulator| async move {
        println!("Testing VM state");

        let state = client
            .protocol_mut()
            .get_vm_state(())
            .await
            .expect("Failed to get VM state")
            .into_inner();

        println!("VM state: {:?}", state);
    })
    .await;
}

/// Test sending a touch event
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_touch_event() {
    spawn_test_emulator(|mut client, _emulator| async move {
        println!("Testing touch event");

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

        client
            .protocol_mut()
            .send_touch(touch_event)
            .await
            .expect("Failed to send touch");

        println!("Touch event sent successfully");

        // Send release event
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

        client
            .protocol_mut()
            .send_touch(release_event)
            .await
            .expect("Failed to send touch release");

        println!("Touch release sent successfully");
    })
    .await;
}

/// Test spawning emulator with custom basic authentication
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_basic_auth() {
    let avd_name = get_test_avd();
    let config = EmulatorConfig::new(avd_name).with_grpc_auth(GrpcAuthConfig::Basic);
    spawn_test_emulator_with_config(config, true, |mut client, _emulator| async move {
        println!("Connected to emulator with basic auth, getting status...");
        let status = client
            .protocol_mut()
            .get_status(())
            .await
            .expect("Failed to get status with basic auth")
            .into_inner();
        println!("Emulator status with basic auth: {:?}", status);
        println!("Basic auth test completed successfully");
    })
    .await;
}

/// Test spawning emulator with custom JWT authentication
#[tokio::test]
#[test_log::test]
#[serial]
async fn test_jwt_auth() {
    let avd_name = get_test_avd();

    // Configure emulator with custom issuer
    let config = EmulatorConfig::new(avd_name)
        .with_grpc_auth(GrpcAuthConfig::Jwt {
            issuer: Some("test-tool".to_string()),
        })
        .with_grpc_port(8555);

    spawn_test_emulator_with_config(config, false, |mut client, _emulator| async move {
        println!("Connected to emulator with JWT auth, getting status...");
        let status = client
            .protocol_mut()
            .get_status(())
            .await
            .expect("Failed to get status with JWT auth")
            .into_inner();

        println!("Emulator status with JWT auth: {:?}", status);
        println!("JWT auth test completed successfully");
    })
    .await;
}

/// Test JWT authentication with protected methods via custom allowlist
#[tokio::test]
//#[test_log::test(default_log_filter = "trace")]
#[test_log::test]
#[serial]
async fn test_jwt_auth_protected() {
    use android_emulator::auth::{AllowlistEntry, GrpcAllowlist};
    use tokio::sync::oneshot;

    let avd_name = get_test_avd();

    // Create a custom allowlist that marks getStatus as "allowed" (requires valid JWT)
    // and getBattery as "protected" (requires JWT with specific aud claim)
    let allowlist = GrpcAllowlist {
        unprotected: vec![],
        allowlist: vec![AllowlistEntry {
            iss: "test-tool".to_string(),
            allowed: vec!["/android.emulation.control.EmulatorController/getStatus".to_string()],
            protected: vec!["/android.emulation.control.EmulatorController/getBattery".to_string()],
        }],
    };

    // Configure emulator with JWT auth and custom allowlist
    let config = EmulatorConfig::new(avd_name)
        .with_grpc_auth(GrpcAuthConfig::Jwt {
            issuer: Some("test-tool".to_string()),
        })
        .with_grpc_port(8556)
        .with_grpc_allowlist(allowlist);

    // Create channels for coordination
    let (token_tx, token_rx) = oneshot::channel::<String>();
    let (terminate_tx, terminate_rx) = oneshot::channel::<()>();

    // Spawn the emulator - the closure exports a token and waits for termination signal
    let emulator_handle = tokio::spawn(async move {
        spawn_test_emulator_with_config(config, false, move |client, _emulator| async move {
            println!("Emulator connected, exporting bearer token...");

            // Export a token with audience claims for the methods we want to test
            let token = client
                .export_token(
                    &["/android.emulation.control.EmulatorController/getBattery"],
                    Duration::from_secs(300),
                )
                .expect("Failed to export token");

            println!("Token exported successfully");

            // Send the token to the test
            token_tx.send(token.token).ok();

            // Wait for signal to terminate
            terminate_rx.await.ok();
            println!("Received termination signal");
        })
        .await;
    });

    // Get the token from the channel
    let token = token_rx.await.expect("Failed to receive token");
    println!("Received token, creating separate client connection...");

    // Create a bearer auth provider with the exported token
    let provider = android_emulator::auth::AuthProvider::new_bearer(token);

    // Connect with the bearer token
    let mut client = EmulatorClient::connect_with_auth("http://localhost:8556", provider)
        .await
        .expect("Failed to connect with bearer token");

    println!("Connected! Testing allowed method (getStatus)...");

    // Test 1: getStatus should work (it's in "allowed" list)
    let status_result = client.protocol_mut().get_status(()).await;
    if let Err(e) = &status_result {
        panic!(
            "getStatus failed, but was expected to succeed as it's in the allowed list: {}",
            e
        );
    }
    println!("getStatus succeeded (allowed method)");

    // Test 2: getBattery should work because our token includes the proper aud claim
    println!("Testing protected method (getBattery)...");
    let battery_result = client.protocol_mut().get_battery(()).await;
    if let Err(e) = &battery_result {
        panic!("protected getBattery request failed: {}", e);
    }
    println!("getBattery succeeded (protected method with proper aud)");

    println!("All access control tests passed!");

    // Signal the emulator to terminate
    terminate_tx.send(()).ok();

    // Wait for the emulator to shut down (this will also dump logs if there was a panic)
    emulator_handle.await.expect("Emulator handle failed");
}
