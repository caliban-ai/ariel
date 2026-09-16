//! Reading and changing a channel's configuration (#18, ADR 0009).
//!
//! A channel's configuration is one gonzalo record
//! ([ADR 0003](../../docs/adr/0003-no-state-of-its-own.md)). Changing it is a
//! read, a patch and an optimistic write: if someone else wrote in between, the
//! operator is shown what is stored rather than silently overwriting it.
//!
//! Every change appends an audit entry. A write that changes nothing does not.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::records::gonzalo::{
    AuditEntry, AuditResult, ChannelConfig, FleetActor, FleetKeyError, FleetRole, Follows,
    NotifyPreset, RecordKey,
};
use crate::records::{Records, RecordsError, Versioned, Write};

/// Which channel, in the shape ADR 0009 keys them by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelKey {
    pub provider: String,
    pub tenant: String,
    pub channel: String,
}

impl ChannelKey {
    pub fn new(
        provider: impl Into<String>,
        tenant: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            tenant: tenant.into(),
            channel: channel.into(),
        }
    }

    fn record_key(&self) -> Result<RecordKey, FleetKeyError> {
        ChannelConfig::key_for(&self.provider, &self.tenant, &self.channel)
    }
}

/// The fields an operator asked to change. `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChannelPatch {
    pub follows: Option<Follows>,
    pub notify: Option<NotifyPreset>,
    pub ceiling: Option<FleetRole>,
}

impl ChannelPatch {
    /// `config` with this patch applied.
    fn onto(&self, config: &ChannelConfig) -> ChannelConfig {
        ChannelConfig {
            follows: self
                .follows
                .clone()
                .unwrap_or_else(|| config.follows.clone()),
            notify: self.notify.unwrap_or(config.notify),
            ceiling: self.ceiling.unwrap_or(config.ceiling),
            ..config.clone()
        }
    }
}

/// What a [`set`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Created(ChannelConfig),
    Updated {
        before: ChannelConfig,
        after: ChannelConfig,
    },
    /// The patch asked for what was already stored.
    Unchanged(ChannelConfig),
    /// Someone else wrote first. Carries the configuration as it now stands,
    /// or `None` if the record was deleted.
    Conflict(Option<ChannelConfig>),
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelsError {
    #[error(transparent)]
    Records(#[from] RecordsError),
    #[error("invalid channel: {0}")]
    Key(#[from] FleetKeyError),
    /// A channel that follows nothing would never hear anything, so ADR 0009
    /// makes `follows` required and gives it no default.
    #[error("a new channel must say what it follows")]
    FollowsRequired,
}

/// The channel's configuration, or `None` if it has none.
pub async fn show(
    records: &Records,
    key: &ChannelKey,
) -> Result<Option<ChannelConfig>, ChannelsError> {
    Ok(read(records, key).await?.map(|stored| stored.value))
}

/// The stored record with its revision, for a later [`apply`].
pub async fn read(
    records: &Records,
    key: &ChannelKey,
) -> Result<Option<Versioned<ChannelConfig>>, ChannelsError> {
    Ok(records.get::<ChannelConfig>(&key.record_key()?).await?)
}

/// Apply `patch` to the channel, creating it if it has no configuration yet.
pub async fn set(
    records: &Records,
    key: &ChannelKey,
    patch: ChannelPatch,
    actor: &FleetActor,
) -> Result<Applied, ChannelsError> {
    match read(records, key).await? {
        Some(stored) => apply(records, stored, patch, actor).await,
        None => create(records, key, patch, actor).await,
    }
}

/// Apply `patch` to a channel already read, failing if it changed since.
pub async fn apply(
    records: &Records,
    stored: Versioned<ChannelConfig>,
    patch: ChannelPatch,
    actor: &FleetActor,
) -> Result<Applied, ChannelsError> {
    let before = stored.value.clone();
    let after = patch.onto(&before);
    if after == before {
        return Ok(Applied::Unchanged(before));
    }

    match records.update(&stored, after.clone()).await? {
        Write::Committed(_) => {
            audit(
                records,
                actor,
                "channel.update",
                &stored.key,
                AuditResult::Succeeded,
            )
            .await?;
            Ok(Applied::Updated { before, after })
        }
        Write::Conflict(current) => {
            audit(
                records,
                actor,
                "channel.update",
                &stored.key,
                AuditResult::Failed("a concurrent edit won".to_owned()),
            )
            .await?;
            Ok(Applied::Conflict(current.map(|stored| stored.value)))
        }
    }
}

async fn create(
    records: &Records,
    key: &ChannelKey,
    patch: ChannelPatch,
    actor: &FleetActor,
) -> Result<Applied, ChannelsError> {
    let follows = patch.follows.ok_or(ChannelsError::FollowsRequired)?;
    let record_key = key.record_key()?;
    let config = ChannelConfig {
        provider: key.provider.clone(),
        tenant: key.tenant.clone(),
        channel: key.channel.clone(),
        follows,
        notify: patch.notify.unwrap_or_default(),
        // ADR 0009: a new channel grants read-only commands until its ceiling
        // is raised on purpose.
        ceiling: patch.ceiling.unwrap_or(FleetRole::Viewer),
    };

    match records.create(record_key.clone(), config.clone()).await? {
        Write::Committed(_) => {
            audit(
                records,
                actor,
                "channel.create",
                &record_key,
                AuditResult::Succeeded,
            )
            .await?;
            Ok(Applied::Created(config))
        }
        Write::Conflict(current) => {
            audit(
                records,
                actor,
                "channel.create",
                &record_key,
                AuditResult::Failed("the channel was configured concurrently".to_owned()),
            )
            .await?;
            Ok(Applied::Conflict(current.map(|stored| stored.value)))
        }
    }
}

async fn audit(
    records: &Records,
    actor: &FleetActor,
    action: &str,
    target: &RecordKey,
    result: AuditResult,
) -> Result<(), RecordsError> {
    records
        .append_audit(&AuditEntry {
            actor: actor.clone(),
            action: action.to_owned(),
            target: target.to_string(),
            at: now_ms(),
            surface_ref: None,
            result,
        })
        .await
        .map(|_| ())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}
