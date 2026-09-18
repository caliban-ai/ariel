//! Log output for `arield` (#49).
//!
//! One line per event on stderr, so it lands in `kubectl logs`. Ariel's own
//! crates log at `info` and everything else at `warn` unless `RUST_LOG` says
//! otherwise; `ARIEL_LOG_FORMAT=json` switches to one JSON object per line for
//! log shipping. Credentials format as `[redacted]`, so nothing logged here
//! carries a secret.

use std::io::IsTerminal;

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// The filter used when `RUST_LOG` is unset or empty.
pub const DEFAULT_FILTER: &str =
    "warn,arield=info,ariel_daemon=info,ariel_core=info,ariel_discord=info";

/// How each event is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable, one line per event.
    Text,
    /// One JSON object per line.
    Json,
}

/// Why logging could not be set up. Names the variable at fault.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("RUST_LOG: {0}")]
    Filter(String),
    #[error("ARIEL_LOG_FORMAT: expected `text` or `json`, got `{0}`")]
    Format(String),
    #[error("cannot install the log subscriber: {0}")]
    Install(String),
}

/// Logging as the environment describes it.
#[derive(Debug)]
pub struct Logging {
    filter: EnvFilter,
    format: Format,
}

impl Logging {
    /// Read `RUST_LOG` and `ARIEL_LOG_FORMAT` through `lookup`.
    ///
    /// # Errors
    ///
    /// A `RUST_LOG` that does not parse, or an unknown `ARIEL_LOG_FORMAT`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, LoggingError> {
        let directives = lookup("RUST_LOG")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_FILTER.to_owned());
        let filter = EnvFilter::try_new(&directives)
            .map_err(|error| LoggingError::Filter(error.to_string()))?;
        let format = match lookup("ARIEL_LOG_FORMAT").as_deref().map(str::trim) {
            None | Some("" | "text") => Format::Text,
            Some("json") => Format::Json,
            Some(other) => return Err(LoggingError::Format(other.to_owned())),
        };
        Ok(Self { filter, format })
    }

    /// The configured output format.
    #[must_use]
    pub fn format(&self) -> Format {
        self.format
    }

    /// A subscriber writing to `writer`, colouring text output when `ansi`.
    pub fn subscriber<W>(self, writer: W, ansi: bool) -> Box<dyn Subscriber + Send + Sync>
    where
        W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    {
        let builder = tracing_subscriber::fmt()
            .with_env_filter(self.filter)
            .with_writer(writer);
        match self.format {
            Format::Text => Box::new(builder.with_ansi(ansi).finish()),
            Format::Json => Box::new(builder.with_ansi(false).json().flatten_event(true).finish()),
        }
    }

    /// Send every event in the process to stderr from now on.
    ///
    /// # Errors
    ///
    /// Another subscriber is already installed.
    pub fn install(self) -> Result<(), LoggingError> {
        let ansi = std::io::stderr().is_terminal();
        tracing::subscriber::set_global_default(self.subscriber(std::io::stderr, ansi))
            .map_err(|error| LoggingError::Install(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::{Format, Logging, LoggingError};

    /// A writer tests can read back.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Everything logged while `body` runs under `pairs` as the environment.
    fn captured(pairs: &[(&str, &str)], body: impl FnOnce()) -> String {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let logging = Logging::from_lookup(|name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
        .unwrap();
        let buffer = Buffer::default();
        let writer = buffer.clone();
        tracing::subscriber::with_default(logging.subscriber(move || writer.clone(), false), body);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    fn emit_samples() {
        tracing::info!(target: "ariel_core::notify", "ariel info");
        tracing::debug!(target: "ariel_core::notify", "ariel debug");
        tracing::info!(target: "arield", "daemon info");
        tracing::info!(target: "hyper::client", "dependency info");
        tracing::warn!(target: "hyper::client", "dependency warning");
    }

    #[test]
    fn by_default_ariel_logs_info_and_dependencies_only_warnings() {
        let out = captured(&[], emit_samples);

        assert!(out.contains("ariel info"), "{out}");
        assert!(out.contains("daemon info"), "{out}");
        assert!(out.contains("dependency warning"), "{out}");
        assert!(!out.contains("ariel debug"), "{out}");
        assert!(!out.contains("dependency info"), "{out}");
    }

    #[test]
    fn rust_log_changes_the_level() {
        let debug = captured(&[("RUST_LOG", "debug")], emit_samples);
        assert!(debug.contains("ariel debug"), "{debug}");
        assert!(debug.contains("dependency info"), "{debug}");

        let quiet = captured(&[("RUST_LOG", "error")], emit_samples);
        assert!(!quiet.contains("ariel info"), "{quiet}");
        assert!(!quiet.contains("dependency warning"), "{quiet}");
    }

    #[test]
    fn an_empty_rust_log_means_the_default() {
        let out = captured(&[("RUST_LOG", "  ")], emit_samples);
        assert!(out.contains("ariel info"), "{out}");
        assert!(!out.contains("ariel debug"), "{out}");
    }

    #[test]
    fn json_format_writes_one_object_per_event() {
        let out = captured(&[("ARIEL_LOG_FORMAT", "json")], || {
            tracing::warn!(target: "ariel_core::notify", channel = "c1", "chat send failed");
        });

        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 1, "{out}");
        let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(event["level"], "WARN");
        assert_eq!(event["message"], "chat send failed");
        assert_eq!(event["channel"], "c1");
    }

    #[test]
    fn text_is_the_default_format() {
        let logging = Logging::from_lookup(|_| None).unwrap();
        assert_eq!(logging.format(), Format::Text);
    }

    #[test]
    fn an_unknown_format_is_an_error_naming_the_variable() {
        let error =
            Logging::from_lookup(|name| (name == "ARIEL_LOG_FORMAT").then(|| "yaml".to_owned()))
                .unwrap_err();
        assert!(matches!(error, LoggingError::Format(_)));
        assert!(error.to_string().contains("ARIEL_LOG_FORMAT"), "{error}");
    }

    #[test]
    fn a_malformed_rust_log_is_an_error_naming_the_variable() {
        let error =
            Logging::from_lookup(|name| (name == "RUST_LOG").then(|| "ariel_core=loud".to_owned()))
                .unwrap_err();
        assert!(error.to_string().starts_with("RUST_LOG"), "{error}");
    }
}
