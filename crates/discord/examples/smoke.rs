//! Manual smoke test against a real Discord guild. See
//! `docs/discord-smoke-test.md`.
//!
//! Registers `/ariel status`, posts one message, connects to the Gateway, and
//! answers every `/ariel status` with "ariel smoke: ok" until stopped.

use std::sync::Arc;

use ariel_core::chat::{
    ChannelRef, ChatProvider, CommandSpec, Destination, Inbound, Message, Role, Visibility,
};
use ariel_discord::{DiscordConfig, DiscordProvider, NAME};
use futures_util::StreamExt;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn env_id(name: &str) -> u64 {
    env(name)
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a Discord ID"))
}

const SPECS: &[CommandSpec] = &[CommandSpec {
    name: "status",
    summary: "Check that Ariel is connected",
    args: &[],
    min_role: Role::Viewer,
}];

#[tokio::main]
async fn main() {
    let guild_id = env_id("DISCORD_GUILD_ID");
    let provider = Arc::new(
        DiscordProvider::new(DiscordConfig {
            token: env("DISCORD_TOKEN"),
            guild_id,
            application_id: env_id("DISCORD_APPLICATION_ID"),
            api_proxy: None,
        })
        .expect("build the Discord provider"),
    );

    provider
        .register_commands(SPECS)
        .await
        .expect("register /ariel status");
    println!("registered /ariel status in guild {guild_id}");

    let channel = Destination::Channel(ChannelRef::new(
        NAME,
        guild_id.to_string(),
        env("DISCORD_CHANNEL_ID"),
    ));
    let posted = provider
        .post(&channel, &Message::text("ariel smoke: connected"))
        .await
        .expect("post to the channel");
    println!("posted message {}", posted.message);

    let mut inbound = provider.inbound();
    tokio::spawn(Arc::clone(&provider).run_gateway());
    println!("listening; run /ariel status in the guild, Ctrl-C to stop");

    while let Some(item) = inbound.next().await {
        if let Inbound::Command(command) = item {
            println!(
                "received /ariel {} from user {}",
                command.name, command.user.user
            );
            let reply = command
                .responder
                .reply(&Message::text("ariel smoke: ok"), Visibility::Public)
                .await;
            println!("replied: {reply:?}");
        }
    }
}
