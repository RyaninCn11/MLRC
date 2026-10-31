use serde_json::Value;

/// Stable, machine-readable error codes.
///
/// The code is the contract with a GUI; the message is plain English. Codes are
/// never renamed or renumbered once shipped - a front-end maps them to its own
/// localized strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Usage,
    Config,
    Io,
    Doctor,
    NotImplemented,
    Network,
    HashMismatch,
    SizeMismatch,
    MetaShape,
    PortableReadOnly,
    VersionUnknown,
    VersionIncomplete,
    JavaMissing,
    JavaTooOld,
    JavaMetaMissing,
    JavaInUse,
    CacheStale,
    LoaderUnavailable,
    ModNotFound,
    ModConflict,
    ModRestricted,
    AuthFailed,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "E_USAGE",
            Self::Config => "E_CONFIG",
            Self::Io => "E_IO",
            Self::Doctor => "E_DOCTOR_FAILED",
            Self::NotImplemented => "E_NOT_IMPLEMENTED",
            Self::Network => "E_NET_HTTP",
            Self::HashMismatch => "E_NET_HASH",
            Self::SizeMismatch => "E_NET_SIZE",
            Self::MetaShape => "E_META_SHAPE",
            Self::PortableReadOnly => "E_PORTABLE_READONLY",
            Self::VersionUnknown => "E_VERSION_UNKNOWN",
            Self::VersionIncomplete => "E_VERSION_INCOMPLETE",
            Self::JavaMissing => "E_JAVA_MISSING",
            Self::JavaTooOld => "E_JAVA_TOO_OLD",
            Self::JavaMetaMissing => "E_JAVA_META_MISSING",
            Self::JavaInUse => "E_JAVA_IN_USE",
            Self::CacheStale => "E_CACHE_STALE",
            Self::LoaderUnavailable => "E_LOADER_UNAVAILABLE",
            Self::ModNotFound => "E_MOD_NOT_FOUND",
            Self::ModConflict => "E_MOD_CONFLICT",
            Self::ModRestricted => "E_MOD_RESTRICTED",
            Self::AuthFailed => "E_AUTH_FAILED",
        }
    }

    /// Process exit code for this error, per the output contract in docs/DOCS.md.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Usage => 2,
            Self::Network | Self::SizeMismatch => 3,
            Self::JavaMissing | Self::JavaTooOld | Self::JavaMetaMissing | Self::JavaInUse => 5,
            Self::MetaShape => 6,
            Self::PortableReadOnly => 7,
            Self::VersionUnknown | Self::VersionIncomplete | Self::LoaderUnavailable => 8,
            Self::ModNotFound | Self::ModConflict | Self::ModRestricted => 9,
            Self::AuthFailed => 4,
            Self::Config
            | Self::Io
            | Self::Doctor
            | Self::NotImplemented
            | Self::HashMismatch
            | Self::CacheStale => 1,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The only error type the engine produces.
#[derive(Debug, thiserror::Error)]
#[error("[{code}]: {message}")]
pub struct MlrcError {
    pub code: ErrorCode,
    pub message: String,
    pub context: Value,
}

impl MlrcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: Value::Null,
        }
    }

    /// Attach structured context for front-ends that want to react to details.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        if !self.context.is_object() {
            self.context = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(map) = &mut self.context {
            map.insert(key.to_string(), value.into());
        }
        self
    }
}

impl From<std::io::Error> for MlrcError {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, err.to_string())
    }
}

pub type MlrcResult<T> = Result<T, MlrcError>;
