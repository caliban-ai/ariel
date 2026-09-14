//! `arield` configuration from the environment (ADR 0008).
//!
//! Credentials never arrive as plain environment variables: each is a file,
//! mounted from a Kubernetes Secret, whose path is named by an
//! `ARIEL_*_TOKEN_FILE` variable. A named file that is missing, unreadable or
//! empty stops `arield` at startup.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

const DISCORD_TOKEN_FILE: &str = "ARIEL_DISCORD_TOKEN_FILE";
const GONZALO_TOKEN_FILE: &str = "ARIEL_GONZALO_TOKEN_FILE";
const HEALTH_ADDR: &str = "ARIEL_HEALTH_ADDR";

/// A credential read from a mounted Secret file. It formats as `[redacted]`,
/// so it cannot leak into logs or error messages.
pub struct Secret(String);

impl Secret {
    /// The credential itself. Never log or format this.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Why `arield` could not start. Messages name variables and paths, never file
/// contents.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{var}: cannot read {}: {source}", path.display())]
    Unreadable {
        var: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{var}: {} is empty", path.display())]
    Empty { var: String, path: PathBuf },
    #[error("ARIEL_HEALTH_ADDR: {value:?} is not a socket address")]
    HealthAddr { value: String },
}

/// Everything `arield` reads at startup.
#[derive(Debug)]
pub struct Config {
    pub discord_token: Option<Secret>,
    pub gonzalo_token: Option<Secret>,
    /// Where `/healthz` is served. Defaults to `0.0.0.0:8081`.
    pub health_addr: SocketAddr,
}

impl Config {
    /// Read configuration through `lookup`, normally `std::env::var`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let secret = |var: &str| {
            lookup(var)
                .map(|path| read_secret(var, PathBuf::from(path)))
                .transpose()
        };
        let discord_token = secret(DISCORD_TOKEN_FILE)?;
        let gonzalo_token = secret(GONZALO_TOKEN_FILE)?;

        let health_addr = match lookup(HEALTH_ADDR) {
            None => SocketAddr::from(([0, 0, 0, 0], 8081)),
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::HealthAddr { value })?,
        };

        Ok(Self {
            discord_token,
            gonzalo_token,
            health_addr,
        })
    }
}

/// Read one credential file, dropping a single trailing newline.
fn read_secret(var: &str, path: PathBuf) -> Result<Secret, ConfigError> {
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(source) => {
            return Err(ConfigError::Unreadable {
                var: var.to_owned(),
                path,
                source,
            });
        }
    };
    let token = raw
        .strip_suffix('\n')
        .map_or(raw.as_str(), |line| line.strip_suffix('\r').unwrap_or(line));
    if token.is_empty() {
        return Err(ConfigError::Empty {
            var: var.to_owned(),
            path,
        });
    }
    Ok(Secret(token.to_owned()))
}
