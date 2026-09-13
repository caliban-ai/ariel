//! `arield` — the long-running Ariel chat bridge.
//!
//! At this stage it only reports its version and the chat providers compiled
//! into the build.

use clap::Parser;

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

fn main() {
    let _args = Args::parse();
    println!("compiled providers: [{}]", compiled_providers().join(", "));
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
