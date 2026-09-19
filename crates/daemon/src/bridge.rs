//! The bridge itself (#19): prospero's fleet in, chat notifications out.
//!
//! [`run`] reads the channel configuration records belonging to the running
//! chat provider (ADR 0009), starts one [`Notifier`] per channel (ADR 0007),
//! and feeds every fleet event to all of them. Each notifier decides for itself
//! whether it follows the event's workspace, so adding a channel is a record,
//! not a code change.
//!
//! **After a restart nothing is replayed.** Ariel stores no cursor
//! ([ADR 0003](../../docs/adr/0003-no-state-of-its-own.md)): the watcher skips
//! agents already terminal at startup, and each running agent gets a fresh live
//! message. Agents that ended while the daemon was down are not notified
//! ([ADR 0011](../../docs/adr/0011-no-replay-after-a-restart.md)).

use std::future::Future;
use std::sync::Arc;

use ariel_core::chat::{ChatProvider, Inbound, ProviderId};
use ariel_core::commands;
use ariel_core::notify::{Notifier, NotifyConfig, Route};
use ariel_core::prospero::types::SessionInfo;
use ariel_core::prospero::{ClientError, FleetWatcher, ProsperoClient, WatchConfig};
use ariel_core::records::gonzalo::ChannelConfig;
use ariel_core::records::{Records, RecordsError};
use futures_util::StreamExt;

/// How many fleet events may queue for one channel before the watcher waits.
const CHANNEL_BUFFER: usize = 256;

/// How many fleet events may queue between the watcher and the fan-out.
const FLEET_BUFFER: usize = 1024;

/// Everything the bridge needs to run.
pub struct Wiring {
    pub provider: Arc<dyn ChatProvider>,
    pub records: Records,
    pub prospero: ProsperoClient,
    pub notify: NotifyConfig,
    pub watch: WatchConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("cannot read channel configuration: {0}")]
    Channels(#[from] RecordsError),
}

/// Every configured channel this provider serves.
///
/// A record naming another provider belongs to a different backend in the same
/// fleet and is skipped, not an error.
pub async fn routes_for(
    records: &Records,
    provider: &ProviderId,
) -> Result<Vec<Route>, RecordsError> {
    let mut routes = Vec::new();
    for key in records.list::<ChannelConfig>().await? {
        let Some(record) = records.get::<ChannelConfig>(&key).await? else {
            continue;
        };
        if record.value.provider != provider.as_str() {
            continue;
        }
        routes.push(Route::from_config(&record.value));
    }
    Ok(routes)
}

/// Run until `shutdown` resolves.
pub async fn run(
    wiring: Wiring,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), BridgeError> {
    let Wiring {
        provider,
        records,
        prospero,
        notify,
        watch,
    } = wiring;

    let routes = routes_for(&records, &provider.id()).await?;
    if routes.is_empty() {
        tracing::warn!(
            provider = provider.id().as_str(),
            "no channel configuration records for this provider; nothing will be notified"
        );
    }
    let channels: Vec<_> = routes
        .iter()
        .map(|route| route.channel().channel.clone())
        .collect();
    tracing::info!(?channels, "notifying configured channels");

    let senders: Vec<_> = routes
        .into_iter()
        .map(|route| Notifier::new(provider.clone(), route, notify.clone()).spawn(CHANNEL_BUFFER))
        .collect();

    // Commands (#16, #20). A registration failure is logged rather than fatal,
    // so notifications keep flowing.
    if let Err(error) = provider.register_commands(commands::ALL).await {
        tracing::warn!(%error, "could not register chat commands");
    }
    let context = commands::Context {
        records: records.clone(),
        prospero: prospero.clone(),
        dashboard: notify.dashboard.clone(),
    };
    let commands = tokio::spawn(dispatch(provider.clone(), context));

    let (mut events, watcher) = FleetWatcher::new(prospero, watch).spawn(FLEET_BUFFER);
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                // Every notifier sees every event and applies its own routing.
                Some(event) => for sender in &senders {
                    if sender.send(event.clone()).await.is_err() {
                        tracing::warn!("a channel notifier stopped; its channel is no longer served");
                    }
                },
                None => break,
            },
            () = &mut shutdown => break,
        }
    }

    // Dropping the senders stops each notifier; dropping the receiver stops the
    // watcher and every agent stream it holds open.
    drop(senders);
    drop(events);
    watcher.abort();
    commands.abort();
    Ok(())
}

/// Log who prosperod takes Ariel to be, so a missing or under-scoped token is
/// obvious at startup rather than as a stream of failed polls. Never fatal:
/// prosperod may simply not be up yet.
pub async fn report_prospero_identity(prospero: &ProsperoClient) {
    match prospero.session().await {
        Ok(SessionInfo::Token {
            token_name, scope, ..
        }) => tracing::info!(%token_name, ?scope, "authenticated to prosperod"),
        Ok(SessionInfo::Disabled) => {
            tracing::info!("prosperod runs with API authentication off");
        }
        Err(error @ ClientError::Auth { .. }) => tracing::error!(
            %error,
            "prosperod refused Ariel's token; set ARIEL_PROSPERO_TOKEN_FILE to a valid token"
        ),
        // prosperod before v0.8 has no /api/session.
        Err(ClientError::Api { status, .. }) if status.as_u16() == 404 => {
            tracing::debug!("prosperod predates API authentication");
        }
        Err(error) => tracing::warn!(%error, "could not ask prosperod who Ariel is"),
    }
}

/// Answer commands as they arrive, each on its own task so a slow one never
/// holds up the next.
async fn dispatch(provider: Arc<dyn ChatProvider>, context: commands::Context) {
    let context = Arc::new(context);
    let mut inbound = provider.inbound();
    while let Some(item) = inbound.next().await {
        let Inbound::Command(command) = item else {
            continue;
        };
        let context = Arc::clone(&context);
        tokio::spawn(async move { commands::respond(&context, &command).await });
    }
}
