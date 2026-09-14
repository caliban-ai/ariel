//! Answering one Discord interaction within its three-second deadline.

use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::{Destination, Message, MessageRef, ProviderError, Responder, Visibility};
use async_trait::async_trait;
use tokio::sync::Mutex;
use twilight_http::Client;
use twilight_model::channel::message::MessageFlags;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseType};
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, InteractionMarker};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::convert::{embed, error};

/// Discord requires an initial response within three seconds; defer a little
/// before that if the core has not answered.
const AUTO_DEFER_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No initial response sent yet.
    Open,
    /// A deferred response is showing; the next reply replaces it.
    Deferred,
    /// The initial response is complete; further replies are follow-ups.
    Answered,
}

pub struct DiscordResponder {
    inner: Arc<Inner>,
}

struct Inner {
    http: Arc<Client>,
    application_id: Id<ApplicationMarker>,
    interaction_id: Id<InteractionMarker>,
    token: String,
    at: Destination,
    state: Mutex<State>,
}

impl DiscordResponder {
    pub fn new(
        http: Arc<Client>,
        application_id: Id<ApplicationMarker>,
        interaction_id: Id<InteractionMarker>,
        token: String,
        at: Destination,
    ) -> Self {
        let inner = Arc::new(Inner {
            http,
            application_id,
            interaction_id,
            token,
            at,
            state: Mutex::new(State::Open),
        });

        // Defer on the core's behalf so a slow command never misses Discord's
        // deadline.
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            tokio::time::sleep(AUTO_DEFER_AFTER).await;
            if let Some(inner) = weak.upgrade()
                && let Err(err) = inner.defer(Visibility::Public).await
            {
                tracing::warn!(%err, "auto-defer of a Discord interaction failed");
            }
        });

        Self { inner }
    }
}

fn flags(visibility: Visibility) -> MessageFlags {
    match visibility {
        Visibility::Public => MessageFlags::empty(),
        Visibility::Private => MessageFlags::EPHEMERAL,
    }
}

impl Inner {
    async fn defer(&self, visibility: Visibility) -> Result<(), ProviderError> {
        let mut state = self.state.lock().await;
        if *state != State::Open {
            return Ok(());
        }
        let response = InteractionResponse {
            kind: InteractionResponseType::DeferredChannelMessageWithSource,
            data: Some(
                InteractionResponseDataBuilder::new()
                    .flags(flags(visibility))
                    .build(),
            ),
        };
        self.http
            .interaction(self.application_id)
            .create_response(self.interaction_id, &self.token, &response)
            .await
            .map_err(error)?;
        *state = State::Deferred;
        Ok(())
    }

    async fn reply(
        &self,
        msg: &Message,
        visibility: Visibility,
    ) -> Result<MessageRef, ProviderError> {
        let embeds = [embed(msg)];
        let interaction = self.http.interaction(self.application_id);
        let mut state = self.state.lock().await;

        let message = match *state {
            State::Open => {
                let response = InteractionResponse {
                    kind: InteractionResponseType::ChannelMessageWithSource,
                    data: Some(
                        InteractionResponseDataBuilder::new()
                            .embeds(embeds.clone())
                            .flags(flags(visibility))
                            .build(),
                    ),
                };
                interaction
                    .create_response(self.interaction_id, &self.token, &response)
                    .await
                    .map_err(error)?;
                // The initial response has no message ID of its own.
                format!("interaction:{}", self.interaction_id)
            }
            State::Deferred if visibility == Visibility::Public => {
                let updated = interaction
                    .update_response(&self.token)
                    .embeds(Some(&embeds))
                    .await
                    .map_err(error)?
                    .model()
                    .await
                    .map_err(|err| ProviderError::Transport(Box::new(err)))?;
                updated.id.to_string()
            }
            State::Deferred | State::Answered => {
                let sent = interaction
                    .create_followup(&self.token)
                    .embeds(&embeds)
                    .flags(flags(visibility))
                    .await
                    .map_err(error)?
                    .model()
                    .await
                    .map_err(|err| ProviderError::Transport(Box::new(err)))?;
                sent.id.to_string()
            }
        };
        *state = State::Answered;

        Ok(MessageRef {
            at: self.at.clone(),
            message,
        })
    }
}

#[async_trait]
impl Responder for DiscordResponder {
    async fn defer(&self, visibility: Visibility) -> Result<(), ProviderError> {
        self.inner.defer(visibility).await
    }

    async fn reply(
        &self,
        msg: &Message,
        visibility: Visibility,
    ) -> Result<MessageRef, ProviderError> {
        self.inner.reply(msg, visibility).await
    }
}
