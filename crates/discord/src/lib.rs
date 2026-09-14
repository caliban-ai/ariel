//! `ariel-discord` — the Discord backend for Ariel, on twilight (ADR 0010).
//!
//! Consumed as an optional dependency behind the `discord` feature, so a build
//! without that feature cannot load Discord. The backend stays thin: it maps
//! the `ChatProvider` trait onto Discord's REST API and Gateway, and owns
//! Discord's interaction deadline (ADR 0006).

mod convert;
mod responder;

use std::sync::{Arc, Mutex, PoisonError};

use ariel_core::chat::{
    Capabilities, ChannelRef, ChatProvider, CommandSpec, Destination, Inbound, Limits, Message,
    MessageRef, ProviderError, ProviderId, SendBudget, ThreadRef, UserRef,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use tokio::sync::mpsc;
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId};
use twilight_http::Client;
use twilight_model::application::command::CommandType;
use twilight_model::application::interaction::Interaction;
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, GuildMarker};
use twilight_util::builder::command::{CommandBuilder, StringBuilder, SubCommandBuilder};

use crate::convert::{channel_id, embed, error, message_id, user_id};
use crate::responder::DiscordResponder;

/// The provider name this backend registers under.
pub const NAME: &str = "discord";

/// Discord's limits (ADR 0006): embed title, description, fields and total.
const LIMITS: Limits = Limits {
    title_chars: 256,
    body_chars: 4096,
    fields: 25,
    field_chars: 1024,
    actions: 25,
    total_chars: 6000,
    send_budget: SendBudget {
        burst: 5,
        per_hour: 3600,
    },
};

/// What the Discord backend needs to run.
pub struct DiscordConfig {
    pub token: String,
    pub guild_id: u64,
    pub application_id: u64,
    /// Send REST calls to this `host:port` over plain HTTP instead of
    /// discord.com. For tests only.
    pub api_proxy: Option<String>,
}

/// Ariel's Discord backend.
pub struct DiscordProvider {
    token: String,
    http: Arc<Client>,
    guild_id: Id<GuildMarker>,
    application_id: Id<ApplicationMarker>,
    inbound_tx: mpsc::UnboundedSender<Inbound>,
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<Inbound>>>,
}

impl DiscordProvider {
    pub fn new(config: DiscordConfig) -> Result<Self, ProviderError> {
        install_crypto_provider();

        let guild_id = Id::new_checked(config.guild_id)
            .ok_or_else(|| ProviderError::InvalidMessage("guild ID must not be 0".to_owned()))?;
        let application_id = Id::new_checked(config.application_id).ok_or_else(|| {
            ProviderError::InvalidMessage("application ID must not be 0".to_owned())
        })?;

        let mut builder = Client::builder().token(config.token.clone());
        if let Some(proxy) = config.api_proxy {
            builder = builder.proxy(proxy, true).ratelimiter(None);
        }

        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        Ok(Self {
            token: config.token,
            http: Arc::new(builder.build()),
            guild_id,
            application_id,
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
        })
    }

    /// Hand the provider an interaction, as the Gateway task does. Anything
    /// other than an Ariel slash command is ignored.
    pub fn deliver_interaction(&self, interaction: Interaction) {
        let Some(channel) = interaction.channel.as_ref() else {
            return;
        };
        let at = Destination::Channel(ChannelRef::new(
            NAME,
            self.guild_id.to_string(),
            channel.id.to_string(),
        ));
        let responder = DiscordResponder::new(
            Arc::clone(&self.http),
            interaction.application_id,
            interaction.id,
            interaction.token.clone(),
            at,
        );
        if let Some(command) = convert::command(&interaction, Box::new(responder)) {
            // Fails only if nobody is reading the inbound stream any more.
            let _ = self.inbound_tx.send(Inbound::Command(command));
        }
    }

    /// Read Discord's Gateway until it closes fatally, delivering
    /// interactions. Interactions need no privileged intents.
    pub async fn run_gateway(self: Arc<Self>) {
        let mut shard = Shard::new(ShardId::ONE, self.token.clone(), Intents::empty());
        let wanted = EventTypeFlags::INTERACTION_CREATE | EventTypeFlags::READY;
        while let Some(item) = twilight_gateway::StreamExt::next_event(&mut shard, wanted).await {
            match item {
                Ok(Event::InteractionCreate(interaction)) => {
                    self.deliver_interaction(interaction.0);
                }
                Ok(Event::Ready(_)) => tracing::info!("connected to the Discord Gateway"),
                Ok(_) => {}
                Err(err) => tracing::warn!(%err, "Discord Gateway receive error"),
            }
        }
        tracing::error!("the Discord Gateway closed and cannot reconnect");
    }
}

/// twilight enables no rustls crypto provider, so install ring before any
/// client is built. Installing twice is harmless.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[async_trait]
impl ChatProvider for DiscordProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(NAME)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::all(LIMITS)
            .with_buttons(false)
            .with_reads_thread_replies(false)
    }

    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError> {
        let mut command = CommandBuilder::new(
            convert::COMMAND,
            "Ariel, the chat bridge for the Caliban agent fleet",
            CommandType::ChatInput,
        );
        for spec in specs {
            let mut sub = SubCommandBuilder::new(spec.name, spec.summary);
            for arg in spec.args {
                sub = sub.option(StringBuilder::new(arg.name, arg.summary).required(arg.required));
            }
            command = command.option(sub);
        }
        self.http
            .interaction(self.application_id)
            .set_guild_commands(self.guild_id, &[command.build()])
            .await
            .map_err(error)?;
        Ok(())
    }

    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError> {
        let channel = channel_id(to)?;
        let sent = self
            .http
            .create_message(channel)
            .embeds(&[embed(msg)])
            .await
            .map_err(error)?
            .model()
            .await
            .map_err(|err| ProviderError::Transport(Box::new(err)))?;
        Ok(MessageRef {
            at: to.clone(),
            message: sent.id.to_string(),
        })
    }

    /// Taken once: a second call returns an empty stream.
    fn inbound(&self) -> BoxStream<'static, Inbound> {
        let receiver = self
            .inbound_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match receiver {
            Some(receiver) => stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|item| (item, receiver))
            })
            .boxed(),
            None => stream::empty().boxed(),
        }
    }

    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError> {
        self.http
            .update_message(channel_id(&target.at)?, message_id(&target.message)?)
            .embeds(Some(&[embed(msg)]))
            .await
            .map_err(error)?;
        Ok(())
    }

    async fn direct_message(
        &self,
        user: &UserRef,
        msg: &Message,
    ) -> Result<MessageRef, ProviderError> {
        let dm = self
            .http
            .create_private_channel(user_id(user)?)
            .await
            .map_err(error)?
            .model()
            .await
            .map_err(|err| ProviderError::Transport(Box::new(err)))?;
        let at = Destination::Channel(ChannelRef::new(
            NAME,
            user.tenant.as_str(),
            dm.id.to_string(),
        ));
        self.post(&at, msg).await
    }

    async fn start_thread(
        &self,
        root: &MessageRef,
        title: &str,
    ) -> Result<ThreadRef, ProviderError> {
        let thread = self
            .http
            .create_thread_from_message(channel_id(&root.at)?, message_id(&root.message)?, title)
            .await
            .map_err(error)?
            .model()
            .await
            .map_err(|err| ProviderError::Transport(Box::new(err)))?;
        Ok(ThreadRef {
            channel: root.at.channel().clone(),
            thread: thread.id.to_string(),
        })
    }
}
