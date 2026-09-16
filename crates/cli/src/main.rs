//! `ariel` — the operator CLI for the Ariel chat bridge.
//!
//! Today it manages channel configuration (#18): which workspaces a channel
//! follows, how much it hears, and the ceiling on commands run in it
//! (ADR 0009). The records live in gonzalo, so the CLI needs either a gonzalod
//! URL or a local store directory.

use std::process::ExitCode;
use std::sync::Arc;

use ariel_core::channels::{self, Applied, ChannelKey, ChannelPatch, ChannelsError};
use ariel_core::records::Records;
use ariel_core::records::gonzalo::{
    ChannelConfig, FleetActor, FleetRole, Follows, FsStore, Identity, NotifyPreset,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Exit code for a write someone else beat us to.
const CONFLICT: u8 = 2;

/// Ariel operator CLI.
#[derive(Debug, Parser)]
#[command(name = "ariel", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage a chat channel's configuration.
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
}

#[derive(Debug, Subcommand)]
enum ChannelAction {
    /// Show a channel's configuration.
    Show {
        #[command(flatten)]
        channel: ChannelArgs,
        #[command(flatten)]
        store: StoreArgs,
    },
    /// Create a channel's configuration, or change the fields you name.
    Set {
        #[command(flatten)]
        channel: ChannelArgs,
        /// `fleet`, or a comma-separated list of workspace names. Required for
        /// a channel that has no configuration yet.
        #[arg(long)]
        follows: Option<String>,
        /// How much the channel hears.
        #[arg(long)]
        notify: Option<NotifyArg>,
        /// The highest role any command in this channel runs with.
        #[arg(long)]
        ceiling: Option<RoleArg>,
        #[command(flatten)]
        store: StoreArgs,
    },
}

#[derive(Debug, Args)]
struct ChannelArgs {
    /// The chat provider, such as `discord`.
    #[arg(long)]
    provider: String,
    /// The guild, workspace or team the channel belongs to.
    #[arg(long)]
    tenant: String,
    /// The platform's channel ID.
    #[arg(long)]
    channel: String,
}

#[derive(Debug, Args)]
struct StoreArgs {
    /// gonzalod's base URL.
    #[arg(long, env = "ARIEL_GONZALO_URL")]
    gonzalo_url: Option<String>,
    /// A local gonzalo store directory, instead of gonzalod.
    #[arg(long, conflicts_with = "gonzalo_url")]
    store: Option<std::path::PathBuf>,
    /// A file holding the bearer token for gonzalod.
    #[arg(long, env = "ARIEL_GONZALO_TOKEN_FILE")]
    token_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NotifyArg {
    All,
    Terminal,
    Failures,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RoleArg {
    Viewer,
    Operator,
    Admin,
}

impl From<NotifyArg> for NotifyPreset {
    fn from(arg: NotifyArg) -> Self {
        match arg {
            NotifyArg::All => NotifyPreset::All,
            NotifyArg::Terminal => NotifyPreset::Terminal,
            NotifyArg::Failures => NotifyPreset::Failures,
        }
    }
}

impl From<RoleArg> for FleetRole {
    fn from(arg: RoleArg) -> Self {
        match arg {
            RoleArg::Viewer => FleetRole::Viewer,
            RoleArg::Operator => FleetRole::Operator,
            RoleArg::Admin => FleetRole::Admin,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("give --gonzalo-url (or ARIEL_GONZALO_URL) or --store")]
    NoStore,
    #[error("--token-file: cannot read {}: {source}", path.display())]
    TokenFile {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("--follows: name at least one workspace, or `fleet`")]
    NoWorkspaces,
    #[error(transparent)]
    Channels(#[from] ChannelsError),
    #[error("{0}")]
    Records(String),
    #[error("{provider}/{tenant}/{channel} is not configured")]
    NotConfigured {
        provider: String,
        tenant: String,
        channel: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ariel: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode, CliError> {
    let Command::Channel { action } = cli.command;
    match action {
        ChannelAction::Show { channel, store } => {
            let records = open(&store)?;
            let key = key(&channel);
            let Some(config) = channels::show(&records, &key).await? else {
                return Err(CliError::NotConfigured {
                    provider: channel.provider,
                    tenant: channel.tenant,
                    channel: channel.channel,
                });
            };
            print!("{}", describe(&config));
            Ok(ExitCode::SUCCESS)
        }
        ChannelAction::Set {
            channel,
            follows,
            notify,
            ceiling,
            store,
        } => {
            let records = open(&store)?;
            let patch = ChannelPatch {
                follows: follows.map(|spec| parse_follows(&spec)).transpose()?,
                notify: notify.map(Into::into),
                ceiling: ceiling.map(Into::into),
            };
            let actor = FleetActor::Service("ariel-cli".to_owned());
            let applied = channels::set(&records, &key(&channel), patch, &actor).await?;
            Ok(report(&applied))
        }
    }
}

fn key(args: &ChannelArgs) -> ChannelKey {
    ChannelKey::new(&args.provider, &args.tenant, &args.channel)
}

/// The records this invocation acts on: gonzalod, or a local store.
fn open(args: &StoreArgs) -> Result<Records, CliError> {
    let author = Identity::new("ariel-cli");
    if let Some(dir) = &args.store {
        return Ok(Records::new(Arc::new(FsStore::new(dir)), author));
    }
    let Some(url) = &args.gonzalo_url else {
        return Err(CliError::NoStore);
    };
    let token = match &args.token_file {
        None => String::new(),
        Some(path) => std::fs::read_to_string(path)
            .map_err(|source| CliError::TokenFile {
                path: path.clone(),
                source,
            })?
            .trim_end()
            .to_owned(),
    };
    Records::connect(url, token, author).map_err(|error| CliError::Records(error.to_string()))
}

/// `fleet`, or a comma-separated list of workspace names.
fn parse_follows(spec: &str) -> Result<Follows, CliError> {
    if spec.trim().eq_ignore_ascii_case("fleet") {
        return Ok(Follows::Fleet);
    }
    let names: Vec<&str> = spec
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    Follows::workspaces(names).map_err(|_| CliError::NoWorkspaces)
}

fn follows_line(follows: &Follows) -> String {
    match follows {
        Follows::Fleet => "fleet".to_owned(),
        Follows::Workspaces { names } => names.iter().cloned().collect::<Vec<_>>().join(", "),
    }
}

fn describe(config: &ChannelConfig) -> String {
    format!(
        "{}/{}/{}\n  follows: {}\n  notify:  {}\n  ceiling: {}\n",
        config.provider,
        config.tenant,
        config.channel,
        follows_line(&config.follows),
        format!("{:?}", config.notify).to_lowercase(),
        format!("{:?}", config.ceiling).to_lowercase(),
    )
}

/// Say what happened, and exit non-zero when nothing was written because
/// someone else got there first.
fn report(applied: &Applied) -> ExitCode {
    match applied {
        Applied::Created(config) => {
            print!("created {}", describe(config));
            ExitCode::SUCCESS
        }
        Applied::Unchanged(config) => {
            print!("unchanged {}", describe(config));
            ExitCode::SUCCESS
        }
        Applied::Updated { before, after } => {
            print!("updated {}", describe(after));
            for line in changes(before, after) {
                println!("  {line}");
            }
            ExitCode::SUCCESS
        }
        Applied::Conflict(current) => {
            eprintln!("ariel: the channel changed since it was read; nothing was written");
            match current {
                Some(config) => eprint!("stored now: {}", describe(config)),
                None => eprintln!("stored now: the channel's configuration was deleted"),
            }
            ExitCode::from(CONFLICT)
        }
    }
}

/// One line per field an update changed, so an operator sees what moved.
fn changes(before: &ChannelConfig, after: &ChannelConfig) -> Vec<String> {
    let mut lines = Vec::new();
    if before.follows != after.follows {
        lines.push(format!(
            "follows: {} -> {}",
            follows_line(&before.follows),
            follows_line(&after.follows)
        ));
    }
    if before.notify != after.notify {
        lines.push(format!("notify: {:?} -> {:?}", before.notify, after.notify));
    }
    if before.ceiling != after.ceiling {
        lines.push(format!("ceiling: {:?} -> {:?}", before.ceiling, after.ceiling).to_lowercase());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_parses_the_fleet_and_workspace_lists() {
        assert_eq!(parse_follows("fleet").unwrap(), Follows::Fleet);
        assert_eq!(parse_follows("  FLEET ").unwrap(), Follows::Fleet);
        assert_eq!(
            parse_follows("caliban, prospero").unwrap(),
            Follows::workspaces(["caliban", "prospero"]).unwrap()
        );
    }

    #[test]
    fn an_empty_follows_list_is_refused() {
        assert!(matches!(
            parse_follows(" , ").unwrap_err(),
            CliError::NoWorkspaces
        ));
    }

    #[test]
    fn changed_fields_are_listed_one_per_line() {
        let before = ChannelConfig {
            provider: "discord".into(),
            tenant: "g".into(),
            channel: "c".into(),
            follows: Follows::Fleet,
            notify: NotifyPreset::All,
            ceiling: FleetRole::Viewer,
        };
        let after = ChannelConfig {
            ceiling: FleetRole::Admin,
            notify: NotifyPreset::Failures,
            ..before.clone()
        };

        let lines = changes(&before, &after);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("ceiling")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("notify")),
            "{lines:?}"
        );
    }
}
