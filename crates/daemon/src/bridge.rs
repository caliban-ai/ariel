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

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::{ChannelRef, ChatProvider, Inbound, ProviderId};
use ariel_core::commands;
use ariel_core::notify::{Notifier, NotifyConfig, Route};
use ariel_core::prospero::types::FleetEvent;
use ariel_core::prospero::types::SessionInfo;
use ariel_core::prospero::{ClientError, FleetWatcher, ProsperoClient, WatchConfig};
use ariel_core::records::gonzalo::ChannelConfig;
use ariel_core::records::{Records, RecordsError};
use futures_util::StreamExt;
use tokio::sync::mpsc;

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
    /// How often to re-read the channel configuration records, so a channel
    /// added or retired while the daemon runs is served without a restart (#56).
    pub channel_reload: Duration,
}

/// How often the channel records are re-read when nothing says otherwise.
pub const CHANNEL_RELOAD: Duration = Duration::from_secs(60);

impl Wiring {
    /// The reload interval, guarding against a zero that would spin the loop.
    fn channel_reload(&self) -> Duration {
        if self.channel_reload.is_zero() {
            CHANNEL_RELOAD
        } else {
            self.channel_reload
        }
    }
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

/// The channels being notified, by channel, each with the route it is serving
/// so a re-read can tell a changed configuration from an unchanged one.
type Served = HashMap<ChannelRef, (Route, mpsc::Sender<FleetEvent>)>;

/// Start a notifier for `route`.
fn start(
    provider: &Arc<dyn ChatProvider>,
    route: Route,
    notify: &NotifyConfig,
) -> mpsc::Sender<FleetEvent> {
    Notifier::new(provider.clone(), route, notify.clone()).spawn(CHANNEL_BUFFER)
}

/// Bring the running notifiers in line with the channel records (#56).
///
/// A record that appeared starts a notifier, one that vanished stops its
/// notifier, and a changed one is restarted so its new `follows`, `notify` or
/// destination applies. A restarted channel loses the live messages it was
/// editing and posts fresh ones, which is the same as what a daemon restart
/// does today.
///
/// **A failed read changes nothing.** gonzalod being briefly unreachable must
/// not tear down every channel; the current routing stays until a read succeeds.
async fn reconcile(
    records: &Records,
    provider: &Arc<dyn ChatProvider>,
    notify: &NotifyConfig,
    served: &mut Served,
) {
    let routes = match routes_for(records, &provider.id()).await {
        Ok(routes) => routes,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not re-read the channel configuration; keeping the channels already served"
            );
            return;
        }
    };

    let wanted: HashMap<ChannelRef, Route> = routes
        .into_iter()
        .map(|route| (route.channel().clone(), route))
        .collect();

    // Gone, or changed: drop the notifier. A changed one is started again below.
    served.retain(|channel, (route, _)| match wanted.get(channel) {
        Some(wanted) if wanted == route => true,
        Some(_) => {
            tracing::info!(channel = %channel.channel, "channel configuration changed; restarting it");
            false
        }
        None => {
            tracing::info!(channel = %channel.channel, "channel is no longer configured; no longer notifying it");
            false
        }
    });

    for (channel, route) in wanted {
        if served.contains_key(&channel) {
            continue;
        }
        tracing::info!(channel = %channel.channel, "now notifying this channel");
        let sender = start(provider, route.clone(), notify);
        served.insert(channel, (route, sender));
    }
}

/// Run until `shutdown` resolves.
pub async fn run(
    wiring: Wiring,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), BridgeError> {
    let channel_reload = wiring.channel_reload();
    let Wiring {
        provider,
        records,
        prospero,
        notify,
        watch,
        ..
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

    let mut served: Served = routes
        .into_iter()
        .map(|route| {
            let channel = route.channel().clone();
            let sender = start(&provider, route.clone(), &notify);
            (channel, (route, sender))
        })
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
    // Channel configuration is a record, not a restart (#56): re-read it on a
    // timer so a channel added, retired or re-scoped takes effect on a running
    // daemon. gonzalo has no watch API, and the record set is tiny.
    let mut reload = tokio::time::interval(channel_reload);
    reload.tick().await; // The first tick is immediate; the set was just read.
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                // Every notifier sees every event and applies its own routing.
                Some(event) => for (route, sender) in served.values() {
                    if sender.send(event.clone()).await.is_err() {
                        tracing::warn!(
                            channel = %route.channel().channel,
                            "a channel notifier stopped; its channel is no longer served"
                        );
                    }
                },
                None => break,
            },
            _ = reload.tick() => reconcile(&records, &provider, &notify, &mut served).await,
            () = &mut shutdown => break,
        }
    }

    // Dropping the senders stops each notifier; dropping the receiver stops the
    // watcher and every agent stream it holds open.
    drop(served);
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

#[cfg(test)]
mod tests {
    use ariel_core::chat::console::ConsoleProvider;
    use ariel_core::records::gonzalo::{Follows, Identity, NotifyPreset};

    use super::*;

    /// gonzalod being unreachable must not tear down the channels already being
    /// served: a failed re-read leaves the routing exactly as it was.
    #[tokio::test]
    async fn a_failed_re_read_keeps_the_channels_already_served() {
        // Nothing listens here, so every read fails.
        let records =
            Records::connect("http://127.0.0.1:1", "token", Identity::new("arield-test")).unwrap();
        let provider: Arc<dyn ChatProvider> = Arc::new(ConsoleProvider::new());
        let notify = NotifyConfig::default();

        let route = Route::new(
            ChannelRef::new("console", "t1", "ops"),
            Follows::Fleet,
            NotifyPreset::All,
        );
        let channel = route.channel().clone();
        let mut served: Served = HashMap::new();
        served.insert(
            channel.clone(),
            (route.clone(), start(&provider, route, &notify)),
        );

        reconcile(&records, &provider, &notify, &mut served).await;

        assert!(
            served.contains_key(&channel),
            "a failed re-read dropped a channel that was being served"
        );
        assert_eq!(served.len(), 1);
    }
}
