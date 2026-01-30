//! Android Emulator gRPC Control Library
//!
//! This library provides Rust bindings for controlling Android Emulators via gRPC,
//! along with utilities for starting and managing emulator instances.

use std::io::{self, BufRead};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use thiserror::Error;

pub mod auth;
pub mod proto;

pub use proto::emulator_controller_client::EmulatorControllerClient;
use tonic::transport::Channel;

use crate::auth::AuthProvider;

#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;

#[derive(Error, Debug)]
pub enum EmulatorError {
    #[error(
        "Android SDK not found. Checked:\n  - ANDROID_HOME environment variable\n  - ANDROID_SDK_ROOT environment variable\n  - Platform default locations (e.g., ~/Android/sdk)\nPlease install the Android SDK or set ANDROID_HOME"
    )]
    AndroidHomeNotFound,

    #[error("No emulator AVDs found")]
    NoAvdsFound,

    #[error("Failed to spawn or connect to ADB server: {0}")]
    AdbError(String),

    #[error("Android SDK emulator tool not found at path: {0}")]
    EmulatorToolNotFound(String),

    #[error("Invalid gRPC endpoint URI: {0}")]
    InvalidUri(String),

    #[error("Failed to enumerate running emulators: {0}")]
    EnumerationFailed(String),

    #[error("Failed to start emulator: {0}")]
    EmulatorStartFailed(String),

    #[error("Emulator connection timed out")]
    ConnectionTimeout,

    #[error("Authentication error: {0}")]
    AuthError(#[from] crate::auth::AuthError),

    #[error("gRPC connection error: {0}")]
    GrpcError(#[from] tonic::transport::Error),

    #[error("gRPC status error: {0}")]
    GrpcStatus(#[from] tonic::Status),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EmulatorError>;

/// Find a free gRPC port by querying running emulators via ADB
///
/// Returns the first available port starting from 8554, or None if unable to enumerate devices.
async fn find_free_grpc_port() -> Option<u16> {
    use adb_client::emulator::ADBEmulatorDevice;
    use std::collections::HashSet;

    let mut server = adb_server().await.ok()?;

    tokio::task::spawn_blocking(move || {
        let devices = match server.devices() {
            Ok(d) => d,
            Err(_) => return None,
        };

        // Collect all ports currently in use
        let mut used_ports = HashSet::new();
        for device in devices {
            if device.identifier.starts_with("emulator-")
                && let Ok(mut emulator_device) = ADBEmulatorDevice::new(device.identifier, None)
                && let Ok(discovery_path) = emulator_device.avd_discovery_path()
                && let Ok(ini_content) = std::fs::read_to_string(&discovery_path)
            {
                let metadata = parse_ini(&ini_content);
                if let Some(port_str) = metadata.get("grpc.port")
                    && let Ok(port) = port_str.parse::<u16>()
                {
                    used_ports.insert(port);
                }
            }
        }

        // Find first available port starting from 8554
        (8554..8600).find(|&port| !used_ports.contains(&port))
    })
    .await
    .ok()?
}

/// Parse and log an emulator output line based on its prefix
fn log_emulator_line(line: &str) {
    let trimmed = line.trim_start();

    if let Some(rest) = trimmed.strip_prefix("ERROR ") {
        tracing::error!("{}", rest.trim_start());
    } else if let Some(rest) = trimmed.strip_prefix("WARNING ") {
        tracing::warn!("{}", rest.trim_start());
    } else if let Some(rest) = trimmed.strip_prefix("WARN ") {
        tracing::warn!("{}", rest.trim_start());
    } else if let Some(rest) = trimmed.strip_prefix("INFO ") {
        tracing::info!("{}", rest.trim_start());
    } else if let Some(rest) = trimmed.strip_prefix("DEBUG ") {
        tracing::debug!("{}", rest.trim_start());
    } else if let Some(rest) = trimmed.strip_prefix("TRACE ") {
        tracing::trace!("{}", rest.trim_start());
    } else {
        // No recognized prefix, log as debug
        tracing::debug!("{}", line);
    }
}

/// gRPC authentication configuration for the emulator
#[derive(Debug, Clone)]
pub enum GrpcAuthConfig {
    /// No authentication required
    None,
    /// Basic authentication using console auth token as bearer token
    Basic,
    /// JWKS + JWT authentication
    Jwt {
        /// Issuer identifier. If None, will be derived from the port as "emulator-{port}"
        issuer: Option<String>,
    },
}

impl Default for GrpcAuthConfig {
    fn default() -> Self {
        GrpcAuthConfig::Jwt { issuer: None }
    }
}

/// Configuration for starting an Android emulator
#[derive(Debug)]
pub struct EmulatorConfig {
    /// Name of the AVD to start. If None, will use the first available AVD.
    avd_name: String,
    /// gRPC port. If None, a free port will be automatically selected.
    grpc_port: Option<u16>,
    /// gRPC authentication configuration
    grpc_auth: GrpcAuthConfig,
    /// Whether to show the emulator window
    no_window: bool,
    /// Whether to load snapshots
    no_snapshot_load: bool,
    /// Whether to save snapshots
    no_snapshot_save: bool,
    /// Whether to show the boot animation
    no_boot_anim: bool,
    /// Whether to disable hardware acceleration (e.g., for running in CI without KVM)
    no_acceleration: bool,
    /// Whether to pass -dalvik-vm-checkjni flag to the emulator
    dalvik_vm_check_jni: bool,
    /// Allow running multiple instances of the same AVD (without support for snapshots)
    read_only: bool,
    /// Optionally quit the emulator after it has booted and been idle for the specified duration (for testing purposes)
    quit_after_boot: Option<Duration>,
    /// Additional command-line arguments
    extra_args: Vec<String>,
    /// Custom allowlist configuration for JWT authentication
    /// If None, a default allowlist will be generated
    grpc_allowlist: Option<auth::GrpcAllowlist>,
    /// Stdout redirect configuration
    stdout: Option<Stdio>,
    /// Stderr redirect configuration
    stderr: Option<Stdio>,
}

impl EmulatorConfig {
    /// Create a new config with a specific AVD name
    pub fn new(avd_name: impl Into<String>) -> Self {
        Self {
            avd_name: avd_name.into(),
            grpc_port: None,
            grpc_auth: GrpcAuthConfig::default(),
            no_window: true,
            no_snapshot_load: false,
            no_snapshot_save: false,
            no_boot_anim: false,
            no_acceleration: false,
            dalvik_vm_check_jni: false,
            read_only: false,
            quit_after_boot: None,
            extra_args: Vec::new(),
            grpc_allowlist: None,
            stdout: None,
            stderr: None,
        }
    }

    /// Get the AVD name that was specified
    pub fn avd_id(&self) -> &str {
        &self.avd_name
    }

    /// Poll ADB until the emulator appears and we can read its metadata
    async fn poll_for_emulator(
        grpc_port: u16,
    ) -> Result<(String, std::collections::HashMap<String, String>, PathBuf)> {
        use adb_client::emulator::ADBEmulatorDevice;

        let mut server = adb_server().await?;
        tokio::task::spawn_blocking(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));

                let devices = match server.devices() {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                for device in devices {
                    if !device.identifier.starts_with("emulator-") {
                        continue;
                    }

                    let mut emulator_device =
                        match ADBEmulatorDevice::new(device.identifier.clone(), None) {
                            Ok(d) => d,
                            Err(_) => continue,
                        };

                    let discovery_path = match emulator_device.avd_discovery_path() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    let ini_content = match std::fs::read_to_string(&discovery_path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    let metadata = parse_ini(&ini_content);

                    // Check if this is our emulator by matching the gRPC port
                    if let Some(port_str) = metadata.get("grpc.port")
                        && let Ok(found_port) = port_str.parse::<u16>()
                        && found_port == grpc_port
                    {
                        return Ok((device.identifier, metadata, discovery_path));
                    }
                }
            }
        })
        .await
        .map_err(|e| EmulatorError::EmulatorStartFailed(format!("Task join error: {}", e)))?
    }

    /// Configure gRPC authentication
    ///
    /// # Examples
    ///
    /// No authentication:
    /// ```no_run
    /// use android_emulator::{EmulatorConfig, GrpcAuthConfig};
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_auth(GrpcAuthConfig::None);
    /// ```
    ///
    /// Basic authentication (console token as bearer):
    /// ```no_run
    /// use android_emulator::{EmulatorConfig, GrpcAuthConfig};
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_auth(GrpcAuthConfig::Basic)
    ///     .with_grpc_port(8554);
    /// ```
    ///
    /// JWT mode with custom issuer:
    /// ```no_run
    /// use android_emulator::{EmulatorConfig, GrpcAuthConfig};
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_auth(GrpcAuthConfig::Jwt {
    ///         issuer: Some("mytool".to_string()),
    ///     })
    ///     .with_grpc_port(8555);
    /// ```
    ///
    /// JWT mode with auto-derived issuer and auto-selected port:
    /// ```no_run
    /// use android_emulator::{EmulatorConfig, GrpcAuthConfig};
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_auth(GrpcAuthConfig::Jwt {
    ///         issuer: None,  // Will be "emulator-{port}"
    ///     });
    /// ```
    pub fn with_grpc_auth(mut self, auth: GrpcAuthConfig) -> Self {
        self.grpc_auth = auth;
        self
    }

    /// Set the gRPC port
    ///
    /// If not specified, a free port will be automatically selected.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use android_emulator::EmulatorConfig;
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_port(8554);
    /// ```
    pub fn with_grpc_port(mut self, port: u16) -> Self {
        self.grpc_port = Some(port);
        self
    }

    /// Configure whether to show the emulator window
    ///
    /// Default is false (headless). Set to true to show the window.
    pub fn with_window(mut self, show: bool) -> Self {
        self.no_window = !show;
        self
    }

    /// Configure whether to load snapshots
    ///
    /// Default is true (load snapshots). Set to false to disable snapshot
    /// loading on startup.
    pub fn with_snapshot_load(mut self, load: bool) -> Self {
        self.no_snapshot_load = !load;
        self
    }

    /// Configure whether to save snapshots on exit
    ///
    /// Default is true (save snapshots). Set to false to disable snapshot
    /// saving on exit.
    pub fn with_snapshot_save(mut self, save: bool) -> Self {
        self.no_snapshot_save = !save;
        self
    }

    /// Configure whether to show the boot animation
    ///
    /// Default is true (show boot animation). Set to false to disable the boot
    /// animation for faster startup.
    pub fn with_boot_animation(mut self, show: bool) -> Self {
        self.no_boot_anim = !show;
        self
    }

    /// Configure whether to disable hardware acceleration (e.g., for running in
    /// CI without KVM)
    ///
    /// Default is false (use hardware acceleration if available). Set to true
    /// to disable hardware acceleration
    pub fn with_acceleration(mut self, enable: bool) -> Self {
        self.no_acceleration = !enable;
        self
    }

    /// Configure whether to pass -dalvik-vm-checkjni flag to the emulator
    ///
    /// This can be useful for testing JNI-related functionality and catching
    /// errors early. Default is false (don't check JNI). Set to true to enable
    /// JNI checking.
    pub fn with_dalvik_vm_check_jni(mut self, enable: bool) -> Self {
        self.dalvik_vm_check_jni = enable;
        self
    }

    /// Configure whether to allow running multiple instances of the same AVD
    /// (without support for snapshots)
    ///
    /// Default is false (don't allow multiple instances). Set to true to allow
    /// multiple instances of the same AVD, but note that snapshots will not
    /// work in this mode.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Configure the emulator to automatically quit after it has booted and
    /// been idle for the specified duration (for testing purposes)
    ///
    /// This can be useful for testing emulator startup and shutdown in CI
    /// environments.
    ///
    /// Default is None (don't quit automatically). Set to Some(duration) to
    /// enable this behavior.
    pub fn with_quit_after_boot(mut self, duration: Option<Duration>) -> Self {
        self.quit_after_boot = duration;
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Set a custom gRPC allowlist configuration for JWT authentication
    ///
    /// This allows fine-grained control over which gRPC methods are accessible
    /// and under what conditions when using JWT authentication. The allowlist
    /// defines three categories of methods:
    ///
    /// - **Unprotected**: Methods that can be invoked without any
    ///   authentication token
    /// - **Allowed**: Methods that can be called with a valid JWT token, even
    ///   without an `aud` claim
    /// - **Protected**: Methods that require the specific method to be present
    ///   in the JWT token's `aud` claim
    ///
    /// If you don't specify a custom allowlist, a default one will be generated
    /// using
    /// [`GrpcAllowlist::default_for_issuer`](auth::GrpcAllowlist::default_for_issuer).
    ///
    /// # Arguments
    ///
    /// * `allowlist` - A [`GrpcAllowlist`](auth::GrpcAllowlist) configuration
    ///
    /// # Example
    ///
    /// ```no_run
    /// use android_emulator::{EmulatorConfig, GrpcAuthConfig, auth::{GrpcAllowlist, AllowlistEntry}};
    ///
    /// let allowlist = GrpcAllowlist {
    ///     unprotected: vec![],  // No methods accessible without auth
    ///     allowlist: vec![
    ///         AllowlistEntry {
    ///             iss: "mytool".to_string(),
    ///             allowed: vec![
    ///                 "/android.emulation.control.EmulatorController/.*".to_string(),
    ///             ],
    ///             protected: vec![
    ///                 "/android.emulation.control.SnapshotService/.*".to_string(),
    ///             ],
    ///         },
    ///     ],
    /// };
    ///
    /// let config = EmulatorConfig::new("test")
    ///     .with_grpc_auth(GrpcAuthConfig::Jwt {
    ///         issuer: Some("mytool".to_string()),
    ///     })
    ///     .with_grpc_allowlist(allowlist);
    /// ```
    ///
    /// # See Also
    ///
    /// - [`with_grpc_auth`](Self::with_grpc_auth) - For configuring
    ///   authentication mode
    /// - [`GrpcAllowlist::default_for_issuer`](auth::GrpcAllowlist::default_for_issuer)
    ///   - For the default allowlist
    pub fn with_grpc_allowlist(mut self, allowlist: auth::GrpcAllowlist) -> Self {
        self.grpc_allowlist = Some(allowlist);
        self
    }

    /// Configure stdout for the emulator process
    pub fn stdout<T: Into<Stdio>>(mut self, cfg: T) -> Self {
        self.stdout = Some(cfg.into());
        self
    }

    /// Configure stderr for the emulator process
    pub fn stderr<T: Into<Stdio>>(mut self, cfg: T) -> Self {
        self.stderr = Some(cfg.into());
        self
    }

    /// Start an Android emulator with the given configuration
    pub async fn spawn(self) -> Result<Emulator> {
        let android_home = get_android_home().await?;
        let emulator_path = android_home.join("emulator").join("emulator");

        if !tokio::fs::try_exists(&emulator_path).await.unwrap_or(false) {
            return Err(EmulatorError::EmulatorToolNotFound(
                emulator_path.display().to_string(),
            ));
        }

        let mut cmd = Command::new(&emulator_path);
        cmd.arg("-avd").arg(&self.avd_name);

        if self.no_window {
            cmd.arg("-no-window");
        }

        if self.no_snapshot_load {
            cmd.arg("-no-snapshot-load");
        }

        if self.no_acceleration {
            cmd.arg("-accel").arg("off");
        }

        if self.no_boot_anim {
            cmd.arg("-no-boot-anim");
        }

        if self.dalvik_vm_check_jni {
            cmd.arg("-dalvik-vm-checkjni");
        }

        if self.read_only {
            cmd.arg("-read-only");
        }

        if let Some(quit_after) = self.quit_after_boot {
            cmd.arg("-quit-after-boot")
                .arg(quit_after.as_secs().to_string());
        }

        // Track whether we're using default piped stdout/stderr for IO forwarding
        let use_default_stdout = self.stdout.is_none();
        let use_default_stderr = self.stderr.is_none();

        // Configure stdout (default to piped and run thread to forward to std::io so test capturing works)
        if let Some(stdout) = self.stdout {
            cmd.stdout(stdout);
        } else {
            cmd.stdout(std::process::Stdio::piped());
        }

        // Configure stderr (default to piped and run thread to forward to std::io so test capturing works)
        if let Some(stderr) = self.stderr {
            cmd.stderr(stderr);
        } else {
            cmd.stderr(std::process::Stdio::piped());
        }

        // Configure gRPC based on authentication mode
        // All modes start with -grpc <port>
        let grpc_port = match self.grpc_port {
            Some(port) => port,
            None => find_free_grpc_port().await.unwrap_or(8554),
        };
        cmd.arg("-grpc").arg(grpc_port.to_string());

        let issuer = match self.grpc_auth {
            GrpcAuthConfig::None => {
                // No additional flags needed
                None
            }
            GrpcAuthConfig::Basic => {
                // Add -grpc-use-token for basic authentication
                cmd.arg("-grpc-use-token");
                None
            }
            GrpcAuthConfig::Jwt { issuer } => {
                // Derive issuer if not provided
                let issuer = issuer.unwrap_or_else(|| format!("emulator-{}", grpc_port));

                // Create allowlist file
                let allowlist = self
                    .grpc_allowlist
                    .unwrap_or_else(|| auth::GrpcAllowlist::default_for_issuer(&issuer));

                // Write allowlist to temp file
                let allowlist_json = serde_json::to_string_pretty(&allowlist).map_err(|e| {
                    EmulatorError::EmulatorStartFailed(format!(
                        "Failed to serialize allowlist: {}",
                        e
                    ))
                })?;

                let temp_dir = std::env::temp_dir();
                let allowlist_path =
                    temp_dir.join(format!("emulator-allowlist-{}.json", std::process::id()));
                tokio::fs::write(&allowlist_path, allowlist_json).await?;

                cmd.arg("-grpc-allowlist").arg(&allowlist_path);

                // Add -grpc-use-jwt for JWT mode
                cmd.arg("-grpc-use-jwt");

                Some(issuer)
            }
        };

        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        let mut process = cmd
            .spawn()
            .map_err(|e| EmulatorError::EmulatorStartFailed(e.to_string()))?;

        // Create IO forwarding thread for stdout if piped (default behavior)
        let stdout_thread = if use_default_stdout {
            let child_out = process.stdout.take().expect("stdout should be piped");

            Some(thread::spawn(move || -> io::Result<()> {
                let _span = tracing::info_span!("emulator").entered();
                let reader = io::BufReader::new(child_out);
                for line in reader.lines() {
                    let line = line?;
                    log_emulator_line(&line);
                }
                Ok(())
            }))
        } else {
            None
        };

        // Create IO forwarding thread for stderr if piped (default behavior)
        let stderr_thread = if use_default_stderr {
            let child_err = process.stderr.take().expect("stderr should be piped");

            Some(thread::spawn(move || -> io::Result<()> {
                let _span = tracing::info_span!("emulator").entered();
                let reader = io::BufReader::new(child_err);
                for line in reader.lines() {
                    let line = line?;
                    log_emulator_line(&line);
                }
                Ok(())
            }))
        } else {
            None
        };

        // Poll ADB until the emulator appears and we can read its metadata
        let (serial, metadata, discovery_path) = Self::poll_for_emulator(grpc_port).await?;

        Ok(Emulator {
            is_owned: true,
            process: tokio::sync::Mutex::new(Some(process)),
            grpc_port,
            serial,
            metadata,
            discovery_path,
            issuer,
            stdout_thread: tokio::sync::Mutex::new(stdout_thread),
            stderr_thread: tokio::sync::Mutex::new(stderr_thread),
        })
    }
}

/// High-level client for controlling an Android Emulator via gRPC
///
/// This is a wrapper around the generated `EmulatorControllerClient` that provides
/// a more convenient API.
pub struct EmulatorClient {
    provider: auth::AuthProvider,
    interceptor: EmulatorControllerClient<
        tonic::service::interceptor::InterceptedService<Channel, auth::AuthProvider>,
    >,
    endpoint: String,
}

impl EmulatorClient {
    /// Try to find an emulator running the specified AVD and connect to it
    pub async fn connect_avd(avd: &str) -> Result<Self> {
        let emulators = list_emulators().await?;
        let matching = emulators
            .into_iter()
            .find(|e| e.avd_id().map(|id| id == avd).unwrap_or(false));
        if let Some(emulator) = matching {
            emulator.connect(Some(Duration::from_secs(30)), true).await
        } else {
            Err(EmulatorError::EmulatorStartFailed(
                "No running emulator found".to_string(),
            ))
        }
    }

    /// Connect to an emulator at the specified endpoint without authentication
    pub async fn connect(endpoint: impl Into<String>) -> Result<Self> {
        let endpoint = endpoint.into();
        let channel = Channel::from_shared(endpoint.clone())
            .map_err(|e| EmulatorError::InvalidUri(e.to_string()))?
            .connect()
            .await?;

        let provider = std::sync::Arc::new(auth::NoOpTokenProvider);
        let provider = auth::AuthProvider::new_with_token_provider(provider);

        Ok(Self {
            interceptor: EmulatorControllerClient::with_interceptor(channel, provider.clone()),
            provider,
            endpoint,
        })
    }

    /// Connect to an emulator with JWT authentication
    pub async fn connect_with_auth(
        endpoint: impl Into<String>,
        provider: auth::AuthProvider,
    ) -> Result<Self> {
        let endpoint = endpoint.into();
        let channel = Channel::from_shared(endpoint.clone())
            .map_err(|e| EmulatorError::InvalidUri(e.to_string()))?
            .connect()
            .await?;

        Ok(Self {
            interceptor: EmulatorControllerClient::with_interceptor(channel, provider.clone()),
            provider,
            endpoint,
        })
    }

    /// Get the authentication scheme used by this client
    pub fn auth_scheme(&self) -> &auth::AuthScheme {
        self.provider.auth_scheme()
    }

    /// Export a bearer token for the given audiences and TTL
    ///
    /// This can be used to export a token with specific, limited audience
    /// claims in order to grant something limited access to the emulator.
    ///
    /// Each audience in `auds` will be included as an `aud` claim in the token
    /// and should correspond to the gRPC method patterns defined in the
    /// allowlist.
    ///
    /// This is only applicable when [`Self::auth_scheme()`] returns
    /// [`auth::AuthScheme::Jwt`].
    pub fn export_token(&self, auds: &[&str], ttl: Duration) -> Result<auth::BearerToken> {
        let token = self.provider.export_token(auds, ttl)?;
        Ok(token)
    }

    /// Get the endpoint this client is connected to
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Get a mutable reference to the generated gRPC client protobuf binding
    pub fn protocol_mut(
        &mut self,
    ) -> &mut EmulatorControllerClient<
        tonic::service::interceptor::InterceptedService<Channel, auth::AuthProvider>,
    > {
        &mut self.interceptor
    }

    /// Get a reference to the generated gRPC client protobuf binding
    pub fn protocol(
        &self,
    ) -> &EmulatorControllerClient<
        tonic::service::interceptor::InterceptedService<Channel, auth::AuthProvider>,
    > {
        &self.interceptor
    }

    /// Wait until the emulator has fully booted
    ///
    /// This method polls the emulator's boot status until it reports as booted,
    /// or until the specified timeout is reached.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum duration to wait for the emulator to boot
    /// * `poll_interval` - Duration to wait between boot status checks (defaults to 2 seconds if None)
    ///
    /// # Returns
    ///
    /// Returns `Ok(Duration)` with the time elapsed until boot completed, or an error if the
    /// timeout is reached or a gRPC error occurs.
    ///
    /// # Errors
    ///
    /// Returns [`EmulatorError::ConnectionTimeout`] if the emulator doesn't boot within the timeout period.
    /// Returns [`EmulatorError::GrpcStatus`] if there's a gRPC communication error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use android_emulator::EmulatorClient;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut client = EmulatorClient::connect("http://localhost:8554").await?;
    ///
    /// // Wait up to 5 minutes for boot, checking every 2 seconds
    /// let elapsed = client.wait_until_booted(Duration::from_secs(300), None).await?;
    /// println!("Emulator booted in {:.1} seconds", elapsed.as_secs_f64());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_until_booted(
        &mut self,
        timeout: Duration,
        poll_interval: Option<Duration>,
    ) -> Result<Duration> {
        let poll_interval = poll_interval.unwrap_or(Duration::from_secs(2));
        let start = std::time::Instant::now();
        let mut attempt = 0;

        loop {
            attempt += 1;
            let status = self.protocol_mut().get_status(()).await?.into_inner();

            if status.booted {
                let elapsed = start.elapsed();
                tracing::info!(
                    "Emulator fully booted after {:.1} seconds ({} attempts)",
                    elapsed.as_secs_f64(),
                    attempt
                );
                return Ok(elapsed);
            }

            tracing::debug!(
                "Boot status: {} (attempt {}, elapsed: {:.1}s)",
                status.booted,
                attempt,
                start.elapsed().as_secs_f64()
            );

            // Check if we've exceeded the timeout
            if start.elapsed() >= timeout {
                return Err(EmulatorError::ConnectionTimeout);
            }

            // Calculate remaining time and sleep for the minimum of poll_interval or remaining time
            let remaining = timeout.saturating_sub(start.elapsed());
            let sleep_duration = poll_interval.min(remaining);

            if sleep_duration.is_zero() {
                return Err(EmulatorError::ConnectionTimeout);
            }

            tokio::time::sleep(sleep_duration).await;
        }
    }
}

/// Handle to a running emulator instance
///
/// This can represent an emulator we spawned with an [`EmulatorConfig`], or an
/// already-running emulator we discovered with [`list_emulators`].
#[derive(Debug)]
pub struct Emulator {
    is_owned: bool,
    process: tokio::sync::Mutex<Option<Child>>,
    serial: String,
    grpc_port: u16,
    /// Path to the discovery .ini file
    discovery_path: PathBuf,
    /// Runtime metadata from the emulator's discovery .ini file
    metadata: std::collections::HashMap<String, String>,
    /// Issuer identifier for JWT authentication (if spawned with custom issuer)
    issuer: Option<String>,
    /// IO forwarding thread for stdout (when piped)
    stdout_thread: tokio::sync::Mutex<Option<JoinHandle<io::Result<()>>>>,
    /// IO forwarding thread for stderr (when piped)
    stderr_thread: tokio::sync::Mutex<Option<JoinHandle<io::Result<()>>>>,
}

impl Emulator {
    /// Get the serial number of this emulator (if known)
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Check if this instance represents an emulator we spawned
    pub fn is_owned(&self) -> bool {
        self.is_owned
    }

    pub fn discovery_path(&self) -> &Path {
        self.discovery_path.as_path()
    }

    /// Get a reference to all emulator metadata from the runtime .ini file
    pub fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }

    /// Get a specific metadata property value
    pub fn get_metadata(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }

    /// Check if this emulator requires JWT authentication
    pub fn requires_jwt_auth(&self) -> bool {
        self.get_metadata("grpc.jwk_active").is_some()
    }

    /// Get the AVD name
    pub fn avd_name(&self) -> Option<&str> {
        self.get_metadata("avd.name")
    }

    /// Get the AVD ID
    pub fn avd_id(&self) -> Option<&str> {
        self.get_metadata("avd.id")
    }

    /// Get the AVD directory path
    pub fn avd_dir(&self) -> Option<&str> {
        self.get_metadata("avd.dir")
    }

    /// Get the emulator version
    pub fn emulator_version(&self) -> Option<&str> {
        self.get_metadata("emulator.version")
    }

    /// Get the emulator build ID
    pub fn emulator_build(&self) -> Option<&str> {
        self.get_metadata("emulator.build")
    }

    /// Get the serial port number
    pub fn port_serial(&self) -> Option<u16> {
        self.get_metadata("port.serial")?.parse().ok()
    }

    /// Get the ADB port number
    pub fn port_adb(&self) -> Option<u16> {
        self.get_metadata("port.adb")?.parse().ok()
    }

    /// Get the command line used to launch the emulator
    pub fn cmdline(&self) -> Option<&str> {
        self.get_metadata("cmdline")
    }

    /// Get the gRPC endpoint URL for this emulator
    pub fn grpc_endpoint(&self) -> String {
        format!("http://localhost:{}", self.grpc_port)
    }

    /// Get the gRPC port
    pub fn grpc_port(&self) -> u16 {
        self.grpc_port
    }

    /// Connect to the emulator's gRPC controller
    ///
    /// This method will automatically retry the connection if it fails, waiting up to the
    /// specified timeout duration. At least one connection attempt is always made before
    /// checking the timeout.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for the connection. If `None`, will wait indefinitely.
    /// * `allow_basic_auth` - If `true`, will use basic auth (grpc.token / console token) if available.
    ///
    /// # Errors
    ///
    /// Returns an error if JWT authentication setup fails.
    pub async fn connect(
        &self,
        timeout: Option<Duration>,
        allow_basic_auth: bool,
    ) -> Result<EmulatorClient> {
        // Check if this emulator requires JWT authentication
        let basic_auth_token = self.get_metadata("grpc.token");

        if self.requires_jwt_auth() {
            tracing::info!(
                "Emulator requires JWT authentication, setting up ES256 token provider..."
            );

            // Connect with JWT authentication
            match self.connect_with_jwt_auth(timeout).await {
                Ok(client) => {
                    tracing::info!("Connected to emulator with JWT authentication.");
                    return Ok(client);
                }
                Err(err) => {
                    tracing::error!("Failed to connect with JWT authentication: {}", err);
                    if basic_auth_token.is_some() && allow_basic_auth {
                        tracing::warn!("Falling back to basic authentication...");
                    } else {
                        return Err(err);
                    }
                }
            }
        } else {
            tracing::info!("Emulator does not require JWT authentication.");
        }

        // Check if this emulator accepts basic auth (grpc.token)
        // This has a lower priority than JWT auth because its mostly only intended for Android Studio use
        if allow_basic_auth && let Some(token) = basic_auth_token {
            tracing::info!("Emulator accepts basic auth, setting up BasicAuthTokenProvider...");
            return self.connect_with_basic_auth(token, timeout).await;
        }

        // Connect without authentication (use no-op provider)
        self.connect_with_noop_auth(timeout).await
    }

    /// Connect with no-op authentication (internal helper for unauth connections)
    async fn connect_with_noop_auth(&self, timeout: Option<Duration>) -> Result<EmulatorClient> {
        let start = std::time::Instant::now();

        let provider = Arc::new(auth::NoOpTokenProvider);
        let provider = AuthProvider::new_with_token_provider(provider);
        loop {
            match EmulatorClient::connect_with_auth(self.grpc_endpoint(), provider.clone()).await {
                Ok(mut client) => {
                    // Try a simple call to verify the connection
                    if client.protocol_mut().get_status(()).await.is_ok() {
                        return Ok(client);
                    }
                }
                Err(err) => {
                    tracing::error!("No-auth connection attempt failed: {}", err);
                }
            }

            // Check timeout after the connection attempt
            if let Some(timeout_duration) = timeout
                && start.elapsed() > timeout_duration
            {
                return Err(EmulatorError::ConnectionTimeout);
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Connect with basic auth token (internal helper for token-based connections)
    async fn connect_with_basic_auth(
        &self,
        token: &str,
        timeout: Option<Duration>,
    ) -> Result<EmulatorClient> {
        let start = std::time::Instant::now();

        let provider = Arc::new(auth::BearerTokenProvider::new(token.to_string()));
        let provider = AuthProvider::new_with_token_provider(provider);

        loop {
            match EmulatorClient::connect_with_auth(self.grpc_endpoint(), provider.clone()).await {
                Ok(mut client) => {
                    // Try a simple call to verify the connection
                    if client.protocol_mut().get_status(()).await.is_ok() {
                        return Ok(client);
                    }
                }
                Err(err) => {
                    tracing::error!("Basic auth connection attempt failed: {}", err);
                }
            }

            // Check timeout after the connection attempt
            if let Some(timeout_duration) = timeout
                && start.elapsed() > timeout_duration
            {
                return Err(EmulatorError::ConnectionTimeout);
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Connect with ES256 authentication (internal helper for JWT connections)
    ///
    /// This will verify the JWT connection works, then return an unauthenticated client
    /// (since after JWT activation, the emulator will accept unauthenticated connections too)
    async fn connect_with_jwt_auth(&self, timeout: Option<Duration>) -> Result<EmulatorClient> {
        // Get the JWKS directory from metadata
        let jwks_path = self.get_metadata("grpc.jwks").ok_or_else(|| {
            EmulatorError::EmulatorStartFailed(
                "Emulator requires JWT auth but grpc.jwks path not found in metadata".to_string(),
            )
        })?;

        let jwks_dir = PathBuf::from(jwks_path);

        let issuer = self.issuer.as_deref().unwrap_or("android-studio");

        // Generate and register key
        let provider = auth::JwtTokenProvider::new_and_register(&jwks_dir, issuer)?;

        // Wait for activation (30 second timeout)
        provider.wait_for_activation(&jwks_dir, Duration::from_secs(10))?;

        let provider = AuthProvider::new_with_token_provider(provider);

        let start = std::time::Instant::now();

        loop {
            tracing::info!("Attempting JWT connection...");
            match EmulatorClient::connect_with_auth(self.grpc_endpoint(), provider.clone()).await {
                Ok(mut client) => {
                    tracing::info!("JWT authentication successful.");
                    // Try a simple call to verify the JWT connection works
                    match client.protocol_mut().get_status(()).await {
                        Ok(_) => {
                            tracing::info!(
                                "Successfully connected to emulator with JWT authentication."
                            );
                            return Ok(client);
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to get status with JWT authentication: {}",
                                err
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("JWT connection attempt failed: {}", err);
                }
            }

            // Check timeout after the connection attempt
            if let Some(timeout_duration) = timeout
                && start.elapsed() > timeout_duration
            {
                return Err(EmulatorError::ConnectionTimeout);
            }
            tracing::info!("Sleeping before retrying JWT connection...");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Terminate the emulator process and wait for it to fully exit
    ///
    /// If this instance owns the process (spawned via `spawn()`), this will kill the process
    /// and wait for it to exit.
    ///
    /// If this instance was discovered via `find()`, this returns an error as we don't own
    /// the process.
    ///
    /// To terminate a discovered emulator, use ADB commands directly.
    pub async fn terminate(&self) -> Result<()> {
        let mut lock = self.process.lock().await;
        if let Some(mut process) = lock.take() {
            return tokio::task::spawn_blocking(move || {
                // Kill the process
                process.kill()?;

                // Wait for the process to fully exit
                let _ = process.wait()?;

                Ok(())
            })
            .await
            .map_err(|e| EmulatorError::EmulatorStartFailed(format!("Task join error: {}", e)))?;
        }

        // TODO: send a shutdown command via gRPC instead and wait for it to exit
        Err(EmulatorError::EmulatorStartFailed(
            "Cannot terminate emulator: process not owned by this instance".to_string(),
        ))
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        // We have exclusive ownership (&mut self), so we can use get_mut() directly
        // without needing to lock, which avoids panics in async contexts
        if let Some(ref mut process) = *self.process.get_mut() {
            let _ = process.kill();
        }

        // Join IO forwarding threads if they exist
        if let Some(out_thread) = self.stdout_thread.get_mut().take() {
            let _ = out_thread.join();
        }
        if let Some(err_thread) = self.stderr_thread.get_mut().take() {
            let _ = err_thread.join();
        }
    }
}

/// Get the Android SDK home directory
///
/// This function tries to find the Android SDK in the following order:
/// 1. ANDROID_HOME environment variable
/// 2. ANDROID_SDK_ROOT environment variable
/// 3. Platform-specific default locations:
///    - Linux: $HOME/Android/sdk or $HOME/Android/Sdk
///    - macOS: $HOME/Library/Android/sdk
///    - Windows: %LOCALAPPDATA%/Android/Sdk
pub async fn get_android_home() -> Result<PathBuf> {
    // Try environment variables first
    if let Ok(path) = std::env::var("ANDROID_HOME") {
        return Ok(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var("ANDROID_SDK_ROOT") {
        return Ok(PathBuf::from(path));
    }

    // Try platform-specific default locations
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            // Try $HOME/Android/sdk first (lowercase)
            let sdk_path = home.join("Android").join("sdk");
            if tokio::fs::try_exists(&sdk_path).await.unwrap_or(false) {
                return Ok(sdk_path);
            }

            // Try $HOME/Android/Sdk (capitalized)
            let sdk_path = home.join("Android").join("Sdk");
            if tokio::fs::try_exists(&sdk_path).await.unwrap_or(false) {
                return Ok(sdk_path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let sdk_path = home.join("Library").join("Android").join("sdk");
            if tokio::fs::try_exists(&sdk_path).await.unwrap_or(false) {
                return Ok(sdk_path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local_data) = dirs::data_local_dir() {
            let sdk_path = local_data.join("Android").join("Sdk");
            if tokio::fs::try_exists(&sdk_path).await.unwrap_or(false) {
                return Ok(sdk_path);
            }
        }
    }

    Err(EmulatorError::AndroidHomeNotFound)
}

/// Find available Android Virtual Devices (AVDs)
pub async fn list_avds() -> Result<Vec<String>> {
    let android_home = get_android_home().await?;

    tokio::task::spawn_blocking(move || {
        let emulator_path = android_home.join("emulator").join("emulator");

        if !emulator_path.exists() {
            return Err(EmulatorError::EmulatorToolNotFound(
                emulator_path.display().to_string(),
            ));
        }

        let output = Command::new(&emulator_path).arg("-list-avds").output()?;

        let avds: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if avds.is_empty() {
            Err(EmulatorError::NoAvdsFound)
        } else {
            Ok(avds)
        }
    })
    .await
    .map_err(|e| EmulatorError::EmulatorStartFailed(format!("Task join error: {}", e)))?
}

/// Parse a simple INI-style file with key=value pairs
fn parse_ini(content: &str) -> std::collections::HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            line.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

async fn adb_server() -> Result<adb_client::server::ADBServer> {
    use adb_client::server::ADBServer;

    let android_home = get_android_home().await?;
    let adb_path = android_home.join("platform-tools").join("adb");
    let adb_path: String = adb_path
        .to_str()
        .ok_or_else(|| EmulatorError::AdbError("Invalid Android home path".to_string()))?
        .to_string();
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5037);

    tokio::task::spawn_blocking(move || Ok(ADBServer::new_from_path(addr, Some(adb_path))))
        .await
        .map_err(|e| EmulatorError::AdbError(format!("Task join error: {}", e)))?
}

/// Enumerates all running emulators that are discoverable via ADB and
/// returns their metadata
///
/// This is useful for finding emulators that were launched outside of this
/// library, such as those launched by Android Studio. It reads the emulator
/// metadata from the discovery path .ini files, which include the gRPC port
/// and authentication settings.
///
/// This does not connect to any emulators; use `connect()` on one of the
/// returned instance to establish a gRPC connection.
pub async fn list_emulators() -> Result<Vec<Emulator>> {
    use adb_client::emulator::ADBEmulatorDevice;

    let mut server = adb_server().await?;

    tokio::task::spawn_blocking(move || {
        let mut emulators = vec![];

        let devices = server.devices().map_err(|e| {
            EmulatorError::IoError(std::io::Error::other(format!(
                "Failed to list ADB devices: {}",
                e
            )))
        })?;

        // Find emulator devices that don't require JWT authentication
        for device in devices {
            if device.identifier.starts_with("emulator-") {
                // Convert to ADBEmulatorDevice to access emulator-specific methods
                let mut emulator_device = ADBEmulatorDevice::new(device.identifier.clone(), None)
                    .map_err(|e| {
                    EmulatorError::IoError(std::io::Error::other(format!(
                        "Failed to create ADBEmulatorDevice: {}",
                        e
                    )))
                })?;

                // Get the discovery path for the runtime metadata
                if let Ok(discovery_path) = emulator_device.avd_discovery_path()
                    && let Ok(ini_content) = std::fs::read_to_string(&discovery_path)
                {
                    let metadata = parse_ini(&ini_content);

                    if let Some(port_str) = metadata.get("grpc.port")
                        && let Ok(grpc_port) = port_str.parse::<u16>()
                    {
                        emulators.push(Emulator {
                            is_owned: false,
                            process: tokio::sync::Mutex::new(None),
                            grpc_port,
                            serial: device.identifier.clone(),
                            metadata,
                            discovery_path: discovery_path.clone(),
                            issuer: None,
                            stdout_thread: tokio::sync::Mutex::new(None),
                            stderr_thread: tokio::sync::Mutex::new(None),
                        });
                    }
                }
            }
        }

        Ok(emulators)
    })
    .await
    .map_err(|e| EmulatorError::EnumerationFailed(format!("Task join error: {}", e)))?
}

/// Try to connect to an emulator, or start a new one if none is running
///
/// This function attempts to connect to the first emulator it can find
/// that's running the same AVD associated with the provided configuration,
/// but if no running emulator is found, it will start a new one with the provided
/// configuration.
///
/// Returns both the connected client and an optional `Emulator` instance
/// representing any newly spawned emulator (which will be `None` if we
/// connected to an existing emulator).
pub async fn connect_or_start_emulator(
    config: EmulatorConfig,
) -> Result<(EmulatorClient, Option<Emulator>)> {
    // Try to connect to existing emulator first
    if let Ok(client) = EmulatorClient::connect_avd(config.avd_id()).await {
        tracing::info!("Connected to existing emulator");
        return Ok((client, None));
    }

    tracing::info!("No existing emulator found, starting new one...");
    // Start a new emulator
    let instance = config.spawn().await?;
    tracing::info!("Emulator started at: {}", instance.grpc_endpoint());

    // Wait for it to be ready and connect
    tracing::info!("Waiting for emulator to be ready...");
    let client = instance
        .connect(Some(Duration::from_secs(120)), true)
        .await?;
    tracing::info!("Connected to new emulator");

    Ok((client, Some(instance)))
}
