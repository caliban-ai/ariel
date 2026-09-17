//! `arield` — the long-running Ariel chat bridge.
//!
//! It loads its configuration, connects to prosperod, gonzalod and the chat
//! platform, notifies every configured channel about the fleet (#19), and
//! serves health checks until it is told to stop.

use std::process::ExitCode;
use std::sync::Arc;

use ariel_core::chat::ChatProvider;
use ariel_core::notify::NotifyConfig;
use ariel_core::prospero::{ProsperoClient, WatchConfig};
use ariel_core::records::Records;
use ariel_core::records::gonzalo::Identity;
use ariel_daemon::bridge::{self, Wiring};
use ariel_daemon::config::{Config, Secret};
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

    let wiring = match wiring(&config) {
        Ok(wiring) => wiring,
        Err(error) => {
            eprintln!("arield: {error}");
            return ExitCode::FAILURE;
        }
    };

    let bridge = async {
        match wiring {
            // Health only: without prosperod, gonzalod and a chat provider
            // there is nothing to bridge, but the process still answers probes.
            None => std::future::pending::<()>().await,
            Some(wiring) => {
                if let Err(error) = bridge::run(wiring, shutdown_signal()).await {
                    eprintln!("arield: {error}");
                }
            }
        }
    };

    tokio::select! {
        result = health::serve(listener) => {
            if let Err(error) = result {
                eprintln!("arield: health endpoint stopped: {error}");
                return ExitCode::FAILURE;
            }
        }
        () = bridge => {}
        () = shutdown_signal() => {}
    }
    ExitCode::SUCCESS
}

/// Why the bridge could not be built. Names variables, never their contents.
#[derive(Debug, thiserror::Error)]
enum WiringError {
    #[error("ARIEL_PROSPERO_URL: {0}")]
    Prospero(String),
    #[error("ARIEL_GONZALO_URL: {0}")]
    Gonzalo(String),
    /// Only a compiled-in backend can fail to build.
    #[cfg(feature = "discord")]
    #[error("{0}")]
    Provider(String),
}

/// The bridge this configuration describes, or `None` when it names no fleet to
/// watch, no records to read, or no chat provider to notify.
fn wiring(config: &Config) -> Result<Option<Wiring>, WiringError> {
    let (Some(prospero_url), Some(gonzalo_url)) = (&config.prospero_url, &config.gonzalo_url)
    else {
        eprintln!(
            "arield: ARIEL_PROSPERO_URL and ARIEL_GONZALO_URL are unset; serving health only"
        );
        return Ok(None);
    };
    let Some(provider) = provider(config)? else {
        eprintln!("arield: no chat provider is configured; serving health only");
        return Ok(None);
    };

    let mut prospero = ProsperoClient::new(prospero_url)
        .map_err(|error| WiringError::Prospero(error.to_string()))?;
    if let Some(token) = &config.prospero_token {
        prospero = prospero.with_token(token.expose());
    }
    let token = config.gonzalo_token.as_ref().map_or("", Secret::expose);
    let records = Records::connect(gonzalo_url, token, Identity::new("arield"))
        .map_err(|error| WiringError::Gonzalo(error.to_string()))?;

    Ok(Some(Wiring {
        provider,
        records,
        prospero,
        notify: NotifyConfig {
            dashboard: config.dashboard_url.clone(),
            ..NotifyConfig::default()
        },
        watch: WatchConfig::default(),
    }))
}

/// The chat provider this build has and this configuration names.
#[cfg(feature = "discord")]
fn provider(config: &Config) -> Result<Option<Arc<dyn ChatProvider>>, WiringError> {
    let (Some(token), Some(guild), Some(application)) = (
        &config.discord_token,
        &config.discord_guild_id,
        &config.discord_application_id,
    ) else {
        return Ok(None);
    };
    let discord = ariel_discord::DiscordProvider::new(ariel_discord::DiscordConfig {
        token: token.expose().to_owned(),
        guild_id: *guild,
        application_id: *application,
        api_proxy: None,
    })
    .map_err(|error| WiringError::Provider(format!("discord: {error}")))?;
    let discord = Arc::new(discord);
    // Commands and interactions arrive over the Gateway connection; without it
    // `/ariel link` would never reach the bridge. Sending needs only REST.
    tokio::spawn(Arc::clone(&discord).run_gateway());
    Ok(Some(discord))
}

#[cfg(not(feature = "discord"))]
fn provider(_config: &Config) -> Result<Option<Arc<dyn ChatProvider>>, WiringError> {
    Ok(None)
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
