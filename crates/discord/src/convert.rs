//! Translations between Ariel's provider-neutral types and Discord's.

use ariel_core::chat::{Args, ChannelRef, Command, Destination, Message, Severity, UserRef};
use twilight_http::error::ErrorType;
use twilight_model::application::interaction::application_command::CommandOptionValue;
use twilight_model::application::interaction::{Interaction, InteractionData};
use twilight_model::channel::message::Embed;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};
use twilight_util::builder::embed::{EmbedBuilder, EmbedFieldBuilder};

use ariel_core::chat::{ProviderError, Responder};

use crate::NAME;

/// The top-level slash command every Ariel subcommand hangs off.
pub const COMMAND: &str = "ariel";

/// A message as a Discord embed.
pub fn embed(msg: &Message) -> Embed {
    let mut builder = EmbedBuilder::new()
        .description(msg.body.clone())
        .color(color(msg.severity));
    if let Some(title) = &msg.title {
        builder = builder.title(title.clone());
    }
    if let Some(link) = &msg.link {
        builder = builder.url(link.as_str());
    }
    for (name, value) in &msg.fields {
        builder = builder.field(EmbedFieldBuilder::new(name.clone(), value.clone()).inline());
    }
    builder.build()
}

fn color(severity: Severity) -> u32 {
    match severity {
        Severity::Info => 0x5865F2,
        Severity::Success => 0x57F287,
        Severity::Warning => 0xFEE75C,
        Severity::Failure => 0xED4245,
    }
}

/// The Discord channel a destination posts into. A thread is itself a channel.
pub fn channel_id(at: &Destination) -> Result<Id<ChannelMarker>, ProviderError> {
    let raw = match at {
        Destination::Channel(channel) => &channel.channel,
        Destination::Thread(thread) => &thread.thread,
    };
    parse(raw, "channel")
}

pub fn message_id(raw: &str) -> Result<Id<MessageMarker>, ProviderError> {
    parse(raw, "message")
}

pub fn user_id(user: &UserRef) -> Result<Id<UserMarker>, ProviderError> {
    parse(&user.user, "user")
}

fn parse<T>(raw: &str, what: &str) -> Result<Id<T>, ProviderError> {
    raw.parse()
        .map_err(|_| ProviderError::InvalidMessage(format!("not a Discord {what} ID: {raw:?}")))
}

/// A twilight REST error as a provider error.
pub fn error(err: twilight_http::Error) -> ProviderError {
    match err.kind() {
        ErrorType::Response { status, .. } if status.get() == 403 => ProviderError::Forbidden,
        ErrorType::Response { status, .. } if status.get() == 404 => ProviderError::NotFound,
        ErrorType::Validation => ProviderError::InvalidMessage(err.to_string()),
        _ => ProviderError::Transport(Box::new(err)),
    }
}

/// An `/ariel <subcommand>` interaction as a `Command`, or `None` if it is not
/// one of Ariel's commands.
pub fn command(interaction: &Interaction, responder: Box<dyn Responder>) -> Option<Command> {
    let Some(InteractionData::ApplicationCommand(data)) = interaction.data.as_ref() else {
        return None;
    };
    if data.name != COMMAND {
        return None;
    }
    let sub = data.options.first()?;
    let CommandOptionValue::SubCommand(options) = &sub.value else {
        return None;
    };
    let args = Args::from_pairs(options.iter().filter_map(|option| match &option.value {
        CommandOptionValue::String(value) => Some((option.name.clone(), value.clone())),
        _ => None,
    }));

    let tenant = interaction.guild_id?.to_string();
    let channel = interaction.channel.as_ref()?.id.to_string();
    let user = interaction.author_id()?.to_string();

    Some(Command {
        name: sub.name.clone(),
        args,
        user: UserRef::new(NAME, tenant.clone(), user),
        at: Destination::Channel(ChannelRef::new(NAME, tenant, channel)),
        responder,
    })
}
