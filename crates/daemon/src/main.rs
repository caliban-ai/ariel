//! `arield` — the long-running Ariel chat bridge.
//!
//! It loads its configuration, reports the chat providers compiled into the
//! build, and serves health checks until it is told to stop. The chat backend
//! and the prospero and gonzalo clients are wired in later (#19).

use std::process::ExitCode;

use ariel_daemon::config::Config;
use ariel_daemon::health;
use clap::Parser;
use tokio::net::TcpListener;

/// Ariel chat bridge daemon.
#[derive(Debug, Parser)]
#[command(name = "arield", version, about)]
struct Args {}

/// Chat providers compiled into this build, one entry per enabled backend
/// feature.
fn compiled_providers() -> &'static [&'static str] {
    &[
        #[cfg(feature = "discord")]
        ariel_discord::NAME,
    ]
}

#[tokio::main]
async fn main() -> ExitCode {
    let _args = Args::parse();

    let config = match Config::from_lookup(|name| std::env::var(name).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("arield: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("compiled providers: [{}]", compiled_providers().join(", "));

    let listener = match TcpListener::bind(config.health_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "arield: cannot serve health checks on {}: {error}",
                config.health_addr
            );
            return ExitCode::FAILURE;
        }
    };

    tokio::select! {
        result = health::serve(listener) => {
            if let Err(error) = result {
                eprintln!("arield: health endpoint stopped: {error}");
                return ExitCode::FAILURE;
            }
        }
        () = shutdown_signal() => {}
    }
    ExitCode::SUCCESS
}

/// Resolves on Ctrl-C, or on SIGTERM, which Kubernetes sends to stop a pod.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::compiled_providers;

    #[test]
    #[cfg(feature = "discord")]
    fn discord_feature_compiles_in_the_discord_provider() {
        assert_eq!(compiled_providers(), ["discord"]);
    }

    #[test]
    #[cfg(not(feature = "discord"))]
    fn no_default_features_compiles_in_no_provider() {
        assert!(compiled_providers().is_empty());
    }
}
