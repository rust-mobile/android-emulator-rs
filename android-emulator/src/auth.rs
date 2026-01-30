use base64::Engine;
use p256::ecdsa::Signature;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer as _;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::metadata::MetadataValue;
use tonic::{Request, Status};

/// Authentication error type
#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Failed to generate key")]
    FailedToGenerateKey,

    #[error("Failed to load key")]
    FailedToLoadKey,

    #[error("Failed to create JWKS directory")]
    FailedToCreateJwksDir(#[source] std::io::Error),

    #[error("Failed to serialize JWKS")]
    FailedToSerializeJwks,

    #[error("Failed to write JWKS file")]
    FailedToWriteJwksFile(#[source] std::io::Error),

    #[error("Timeout waiting for JWK registration")]
    JwksRegistrationTimeout,

    #[error("Unspecified signing error")]
    Unspecified,
}

/// Authentication scheme for gRPC connections
#[derive(Debug)]
pub enum AuthScheme {
    /// No authentication
    None,
    /// A static bearer token is attached to each request
    Bearer,
    /// JWT tokens are attached to each request (these have an `iss` field that
    /// matches the issuer, an `aud` field that matches the method being
    /// requested, and are signed with an ES256 key that has been registered
    /// with the emulator)
    Jwt { issuer: String, jwks_dir: PathBuf },
}

/// Bearer token with expiration
pub struct BearerToken {
    pub token: String,
    pub expires_at: SystemTime,
}

/// Trait for providing JWT tokens
pub(crate) trait TokenProvider: Send + Sync + 'static {
    fn auth_scheme(&self) -> &AuthScheme;
    fn token_for_aud(&self, aud: &str) -> Result<String, AuthError>;
    fn export_token(&self, _auds: &[&str], _ttl: Duration) -> Result<BearerToken, AuthError> {
        Err(AuthError::Unsupported("export_token".into()))
    }
}

/// No-op token provider for unauthenticated connections
#[derive(Clone)]
pub struct NoOpTokenProvider;

impl TokenProvider for NoOpTokenProvider {
    fn auth_scheme(&self) -> &AuthScheme {
        &AuthScheme::None
    }
    fn token_for_aud(&self, _aud: &str) -> Result<String, AuthError> {
        Ok(String::new())
    }
}

/// Static bearer token provider for emulators with grpc.token, or for clients
/// that have been given a static JWT token to use.
#[derive(Clone)]
pub struct BearerTokenProvider {
    token: String,
}

impl BearerTokenProvider {
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

impl TokenProvider for BearerTokenProvider {
    fn auth_scheme(&self) -> &AuthScheme {
        &AuthScheme::Bearer
    }
    fn token_for_aud(&self, _aud: &str) -> Result<String, AuthError> {
        Ok(self.token.clone())
    }
}

/// Allowlist entry for JWT authentication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub iss: String,
    pub allowed: Vec<String>,
    pub protected: Vec<String>,
}

/// gRPC allowlist configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcAllowlist {
    pub unprotected: Vec<String>,
    pub allowlist: Vec<AllowlistEntry>,
}

impl GrpcAllowlist {
    /// Create a default allowlist for the given issuer
    pub fn default_for_issuer(issuer: &str) -> Self {
        Self {
            unprotected: vec![],
            allowlist: vec![
                // Always include android-studio to not break embedded emulator
                AllowlistEntry {
                    iss: "android-studio".to_string(),
                    allowed: vec![
                        "/android.emulation.control.EmulatorController/.*".to_string(),
                        "/android.emulation.control.UiController/.*".to_string(),
                        "/android.emulation.control.SnapshotService/.*".to_string(),
                        "/android.emulation.control.incubating.*".to_string(),
                    ],
                    protected: vec![],
                },
                // Add custom issuer if it's not android-studio
                AllowlistEntry {
                    iss: issuer.to_string(),
                    allowed: vec![
                        "/android.emulation.control.EmulatorController/.*".to_string(),
                        "/android.emulation.control.UiController/.*".to_string(),
                        "/android.emulation.control.SnapshotService/.*".to_string(),
                        "/android.emulation.control.incubating.*".to_string(),
                    ],
                    protected: vec![],
                },
            ],
        }
    }
}

/// JWT header for ES256 (ECDSA P-256)
#[derive(Serialize)]
struct JwtHeader {
    alg: String,
    kid: String,
}

/// JWT claims
#[derive(Serialize)]
struct JwtClaims {
    iss: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aud: Vec<String>,
    exp: u64,
    iat: u64,
}

/// JWK format for ECDSA P-256 public key
#[derive(Serialize, Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    x: String,
    y: String,
    kid: String,
    #[serde(rename = "use")]
    use_: String,
    alg: String,
    key_ops: Vec<String>,
}

/// JWKS file format
#[derive(Serialize, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

/// Cached token with expiration
struct CachedToken {
    token: String,
    expires_at: SystemTime,
}

/// JWT ES256 token provider implementation
pub struct JwtTokenProvider {
    auth_scheme: AuthScheme,
    signing_key: SigningKey,
    key_id: String,
    issuer: String,
    token_cache: Mutex<HashMap<String, CachedToken>>,
}

impl JwtTokenProvider {
    /// Create a new JWT ES256 token provider and register the key with the emulator
    pub fn new_and_register(
        jwks_dir: impl Into<PathBuf>,
        issuer: impl Into<String>,
    ) -> Result<Arc<Self>, AuthError> {
        let jwks_dir = jwks_dir.into();
        let issuer = issuer.into();

        // Generate P-256 signing key (private key)
        let signing_key = SigningKey::random(&mut OsRng);

        // Public key and x/y extraction (uncompressed SEC1 point)
        let verify_key = signing_key.verifying_key();
        let encoded = verify_key.to_encoded_point(false);
        let public_key_bytes = encoded.as_bytes();

        // Generate a random key ID
        let key_id = uuid::Uuid::new_v4().to_string();

        // For ECDSA P-256, the public key is 65 bytes: 0x04 + 32 bytes X + 32 bytes Y
        if public_key_bytes.len() != 65 || public_key_bytes[0] != 0x04 {
            return Err(AuthError::FailedToGenerateKey);
        }

        let x_bytes = &public_key_bytes[1..33];
        let y_bytes = &public_key_bytes[33..65];

        // Base64 URL encode without padding
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(x_bytes);
        let y = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(y_bytes);

        let jwk = Jwk {
            kid: key_id.clone(),
            alg: "ES256".to_string(),
            kty: "EC".to_string(),
            crv: "P-256".to_string(),
            x,
            y,
            use_: "sig".to_string(),
            key_ops: vec!["verify".to_string()],
        };

        // Create JWKS with single key
        let jwks = Jwks { keys: vec![jwk] };

        // Write JWKS file
        std::fs::create_dir_all(&jwks_dir).map_err(AuthError::FailedToCreateJwksDir)?;
        let jwks_path = jwks_dir.join(format!("{}.jwk", key_id));
        let jwks_json =
            serde_json::to_string_pretty(&jwks).map_err(|_e| AuthError::FailedToSerializeJwks)?;
        tracing::debug!("Writing JWK to {}", jwks_path.display());
        std::fs::write(&jwks_path, jwks_json).map_err(AuthError::FailedToWriteJwksFile)?;

        Ok(Arc::new(Self {
            auth_scheme: AuthScheme::Jwt {
                issuer: issuer.to_string(),
                jwks_dir: jwks_dir.to_path_buf(),
            },
            signing_key,
            key_id,
            issuer: issuer.to_string(),
            token_cache: Mutex::new(HashMap::new()),
        }))
    }

    /// Wait for the emulator to activate this key
    pub fn wait_for_activation(
        self: &Arc<Self>,
        jwks_dir: &Path,
        timeout: Duration,
    ) -> Result<(), AuthError> {
        let active_jwk_path = jwks_dir.join("active.jwk");
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                return Err(AuthError::JwksRegistrationTimeout);
            }

            // Read active.jwk if it exists
            if let Ok(contents) = std::fs::read_to_string(&active_jwk_path)
                && let Ok(jwks) = serde_json::from_str::<Jwks>(&contents)
            {
                // Check if our key ID is in the active keys
                if jwks.keys.iter().any(|k| k.kid == self.key_id) {
                    return Ok(());
                }
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn create_token(&self, auds: &[&str], ttl: Duration) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::Unspecified)?
            .as_secs();
        let exp = now + ttl.as_secs();

        let header = JwtHeader {
            alg: "ES256".to_string(),
            kid: self.key_id.clone(),
        };

        let claims = JwtClaims {
            iss: self.issuer.clone(),
            aud: auds.iter().map(|&s| s.to_string()).collect(),
            exp,
            iat: now,
        };

        // Encode header and claims
        let header_json = serde_json::to_string(&header).map_err(|_| AuthError::Unspecified)?;
        let claims_json = serde_json::to_string(&claims).map_err(|_| AuthError::Unspecified)?;

        let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header_json);
        let claims_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims_json);

        // Create signing input
        let signing_input = format!("{}.{}", header_b64, claims_b64);

        // Sign with ECDSA P-256
        let sig: Signature = self.signing_key.sign(signing_input.as_bytes());
        let sig_bytes = sig.to_bytes();
        let signature_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);

        // Create final JWT
        let token = format!("{}.{}", signing_input, signature_b64);
        Ok(token)
    }
}

impl TokenProvider for JwtTokenProvider {
    fn auth_scheme(&self) -> &AuthScheme {
        &self.auth_scheme
    }
    fn token_for_aud(&self, aud: &str) -> Result<String, AuthError> {
        let mut cache = self.token_cache.lock().unwrap();
        let now = SystemTime::now();

        // Check if we have a valid cached token
        if let Some(cached) = cache.get(aud)
            && cached.expires_at > now
        {
            return Ok(cached.token.clone());
        }

        // Create new token with 10 minute TTL
        let ttl = Duration::from_secs(10 * 60);
        let token = self.create_token(&[aud], ttl)?;

        // Cache it (expires 30 seconds before actual expiration for safety)
        let expires_at = now + ttl - Duration::from_secs(30);
        cache.insert(
            aud.to_string(),
            CachedToken {
                token: token.clone(),
                expires_at,
            },
        );

        Ok(token)
    }
    fn export_token(&self, auds: &[&str], ttl: Duration) -> Result<BearerToken, AuthError> {
        let token = self.create_token(auds, ttl)?;

        let expires_at = SystemTime::now() + ttl;
        Ok(BearerToken { token, expires_at })
    }
}

/// An authentication interceptor for gRPC requests, responsible for adding
/// an appropriate authorization header to each request.
#[derive(Clone)]
pub struct AuthProvider {
    provider: Arc<dyn TokenProvider>,
}

impl AuthProvider {
    pub(crate) fn new_with_token_provider(provider: Arc<dyn TokenProvider>) -> Self {
        Self { provider }
    }

    pub fn new_no_auth() -> Self {
        Self {
            provider: Arc::new(NoOpTokenProvider),
        }
    }
    pub fn new_bearer(token: impl Into<String>) -> Self {
        Self {
            provider: Arc::new(BearerTokenProvider::new(token.into())),
        }
    }
    pub async fn new_jwt(
        jwks_dir: impl Into<PathBuf>,
        issuer: impl Into<String>,
    ) -> Result<Arc<Self>, AuthError> {
        let jwks_dir = jwks_dir.into();
        let issuer = issuer.into();
        let jwt_provider = tokio::task::spawn_blocking(move || {
            JwtTokenProvider::new_and_register(jwks_dir, issuer)
        })
        .await
        .map_err(|_e| AuthError::Unspecified)??;
        Ok(Arc::new(Self {
            provider: jwt_provider,
        }))
    }

    pub fn auth_scheme(&self) -> &AuthScheme {
        self.provider.auth_scheme()
    }
    pub fn export_token(&self, auds: &[&str], ttl: Duration) -> Result<BearerToken, AuthError> {
        self.provider.export_token(auds, ttl)
    }
}

impl tonic::service::Interceptor for AuthProvider {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        // The audience for JWT is the gRPC method path
        let method = req
            .extensions()
            .get::<tonic::GrpcMethod>()
            .expect("GrpcMethod missing");
        let aud = format!("/{}/{}", method.service(), method.method());
        let token = self
            .provider
            .token_for_aud(&aud)
            .map_err(|e| Status::unauthenticated(format!("Token error: {e}")))?;

        // Only add authorization header if we have a token (skip for no-op provider)
        if !token.is_empty() {
            // Add Bearer token to authorization header
            let value = MetadataValue::try_from(format!("Bearer {token}"))
                .map_err(|_| Status::internal("Invalid auth metadata"))?;

            req.metadata_mut().insert("authorization", value);
        }
        Ok(req)
    }
}
