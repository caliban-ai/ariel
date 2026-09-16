//! Account linking: a one-time token that ties a chat account to a person (#16).
//!
//! An administrator **mints** a token with `ariel link new`, carrying the role and
//! scope the person will get. The person **redeems** it in chat with
//! `/ariel link <token>`. Redemption binds their chat account to a person (new, or
//! one the token names), grants the role, and marks the token consumed.
//!
//! - **Only a hash of the token is stored** (gonzalo ADR 0022). The token itself is
//!   shown once, when minted, and appears in no record or audit entry.
//! - **A token works once.** Consuming it is an optimistic write against the
//!   revision that was read, so two concurrent redemptions give one success and
//!   one [`LinkError::AlreadyUsed`].
//! - **Tokens are marked consumed, never deleted.** gonzalo deletes are local, so a
//!   sync from a peer would resurrect a deleted token (gonzalo ADR 0018).
//! - **Every redemption attempt is audited**, successful or not, naming the
//!   account that tried.

use std::time::Duration;

use crate::chat::{
    ArgSpec, Command, CommandSpec, Message, ProviderId, Role, Severity, UserRef, Visibility,
};
use crate::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, BindingOrigin, FleetActor, FleetKeyError, FleetRole,
    GrantScope, IdentityBinding, LinkSecret, LinkToken, Person, RecordKey, RedeemError, RoleGrant,
};
use crate::records::{Records, RecordsError, Write};

/// How many fresh secrets [`mint`] tries if one collides with a stored token.
const MINT_ATTEMPTS: u32 = 3;

/// What an administrator asks a new token to grant.
#[derive(Debug, Clone)]
pub struct MintRequest {
    pub role: FleetRole,
    pub scope: GrantScope,
    /// Link to this existing person, or `None` to create one on redemption.
    pub person: Option<String>,
    /// How long the token stays redeemable.
    pub ttl: Duration,
    pub minted_by: FleetActor,
}

/// A freshly minted token.
pub struct Minted {
    /// The token to hand to the person. **Shown once and never stored.**
    pub token: String,
    /// Where the token's hash is stored.
    pub key: RecordKey,
    /// When it stops being redeemable, in ms since the Unix epoch.
    pub expires_at: i64,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("token", &"[redacted]")
            .field("key", &self.key)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A successful redemption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub person: String,
    pub role: FleetRole,
    pub scope: GrantScope,
}

/// Why a token could not be minted or redeemed. No variant carries the token.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// Malformed, unknown, or not matching: deliberately indistinguishable, so a
    /// guess learns nothing.
    #[error("that link token is not valid")]
    Invalid,
    #[error("that link token has expired")]
    Expired,
    #[error("that link token has already been used")]
    AlreadyUsed,
    #[error("this account is already linked")]
    AlreadyLinked,
    #[error("that link token was issued to someone else")]
    PersonMismatch,
    #[error(transparent)]
    Records(#[from] RecordsError),
    #[error("invalid record key: {0}")]
    Key(#[from] FleetKeyError),
    #[error("no randomness for a link token: {0}")]
    Random(getrandom::Error),
    #[error("could not find an unused token after {0} attempts")]
    Collisions(u32),
}

impl LinkError {
    /// A refusal the person caused, as opposed to a failure of the system.
    fn is_refusal(&self) -> bool {
        matches!(
            self,
            LinkError::Invalid
                | LinkError::Expired
                | LinkError::AlreadyUsed
                | LinkError::AlreadyLinked
                | LinkError::PersonMismatch
        )
    }
}

/// Mint a token, store its hash, and return the token itself.
pub async fn mint(
    records: &Records,
    request: MintRequest,
    now_ms: i64,
) -> Result<Minted, LinkError> {
    let expires_at =
        now_ms.saturating_add(i64::try_from(request.ttl.as_millis()).unwrap_or(i64::MAX));

    for _ in 0..MINT_ATTEMPTS {
        let secret = LinkSecret::from_bytes(random_bytes()?);
        let token = LinkToken::new(
            &secret,
            request.role,
            request.scope.clone(),
            request.person.clone(),
            request.minted_by.clone(),
            now_ms,
            expires_at,
        );
        let key = token.key();
        if let Write::Committed(_) = records.create(key.clone(), token).await? {
            audit(
                records,
                request.minted_by.clone(),
                "link.mint",
                &key,
                now_ms,
                AuditResult::Succeeded,
            )
            .await?;
            return Ok(Minted {
                token: secret.to_hex(),
                key,
                expires_at,
            });
        }
    }
    Err(LinkError::Collisions(MINT_ATTEMPTS))
}

/// Redeem `token` for the chat account `user`, and audit the attempt.
pub async fn redeem(
    records: &Records,
    token: &str,
    user: &UserRef,
    handle: Option<&str>,
    now_ms: i64,
) -> Result<Linked, LinkError> {
    let authenticator = authenticator(&user.provider);
    let binding_key = IdentityBinding::key_for(&authenticator, &user.user)?;

    let outcome = attempt(
        records,
        token,
        user,
        handle,
        &authenticator,
        &binding_key,
        now_ms,
    )
    .await;

    let (actor, result) = match &outcome {
        Ok(linked) => (
            FleetActor::Person(linked.person.clone()),
            AuditResult::Succeeded,
        ),
        Err(error) => (
            FleetActor::Unlinked {
                authenticator: authenticator.clone(),
                subject: user.user.clone(),
            },
            if error.is_refusal() {
                AuditResult::Denied
            } else {
                AuditResult::Failed(error.to_string())
            },
        ),
    };
    audit(records, actor, "link.redeem", &binding_key, now_ms, result).await?;
    outcome
}

async fn attempt(
    records: &Records,
    token: &str,
    user: &UserRef,
    handle: Option<&str>,
    authenticator: &Authenticator,
    binding_key: &RecordKey,
    now_ms: i64,
) -> Result<Linked, LinkError> {
    let secret = LinkSecret::parse(token.trim()).map_err(|_| LinkError::Invalid)?;
    let stored = records
        .get::<LinkToken>(&LinkToken::key_for_secret(&secret))
        .await?
        .ok_or(LinkError::Invalid)?;

    // Checked before consuming anything, so a mistaken attempt from an account
    // that is already linked leaves the token usable.
    if records.get::<IdentityBinding>(binding_key).await?.is_some() {
        return Err(LinkError::AlreadyLinked);
    }

    let new_person = stored.value.person.is_none();
    let person = match &stored.value.person {
        Some(person) => person.clone(),
        None => new_person_id()?,
    };

    let consumed = stored
        .value
        .redeem(&secret, &person, binding_key.clone(), now_ms)
        .map_err(|error| match error {
            RedeemError::WrongSecret => LinkError::Invalid,
            RedeemError::Expired => LinkError::Expired,
            RedeemError::AlreadyConsumed => LinkError::AlreadyUsed,
            RedeemError::PersonMismatch => LinkError::PersonMismatch,
        })?;

    // The single-use guarantee: only one writer can move the token from the
    // revision both read to a consumed one.
    if let Write::Conflict(_) = records.update(&stored, consumed).await? {
        return Err(LinkError::AlreadyUsed);
    }

    if new_person {
        let display_name = handle.unwrap_or(&user.user).to_owned();
        let _ = records
            .create(
                Person::key(&person)?,
                Person {
                    display_name,
                    email: None,
                },
            )
            .await?;
    }

    let binding = IdentityBinding {
        authenticator: authenticator.clone(),
        subject: user.user.clone(),
        person: person.clone(),
        handle: handle.map(str::to_owned),
        email: None,
        bound_at: now_ms,
        bound_by: BindingOrigin::LinkToken {
            token_hash: stored.value.token_hash.clone(),
        },
    };
    if let Write::Conflict(_) = records.create(binding_key.clone(), binding).await? {
        return Err(LinkError::AlreadyLinked);
    }

    grant(records, &person, &stored.value, now_ms).await?;

    Ok(Linked {
        person,
        role: stored.value.role,
        scope: stored.value.scope.clone(),
    })
}

/// Give `person` the token's role in the token's scope, replacing any grant they
/// already hold there: the minter chose this role on purpose.
async fn grant(
    records: &Records,
    person: &str,
    token: &LinkToken,
    now_ms: i64,
) -> Result<(), LinkError> {
    let key = RoleGrant::key_for(person, &token.scope)?;
    let grant = RoleGrant {
        person: person.to_owned(),
        scope: token.scope.clone(),
        role: token.role,
        granted_by: token.minted_by.clone(),
        granted_at: now_ms,
    };
    match records.get::<RoleGrant>(&key).await? {
        None => {
            let _ = records.create(key, grant).await?;
        }
        Some(existing) => {
            let _ = records.update(&existing, grant).await?;
        }
    }
    Ok(())
}

/// The authenticator that vouches for accounts on this chat provider.
fn authenticator(provider: &ProviderId) -> Authenticator {
    match provider.as_str() {
        "discord" => Authenticator::Discord,
        "slack" => Authenticator::Slack,
        "teams" => Authenticator::Teams,
        other => Authenticator::Other(other.to_owned()),
    }
}

/// A fresh person id within gonzalo's `[A-Za-z0-9_-]{1,64}`.
fn new_person_id() -> Result<String, LinkError> {
    let bytes = random_bytes()?;
    let hex: String = bytes[..10].iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("p-{hex}"))
}

fn random_bytes() -> Result<[u8; 32], LinkError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(LinkError::Random)?;
    Ok(bytes)
}

async fn audit(
    records: &Records,
    actor: FleetActor,
    action: &str,
    target: &RecordKey,
    at: i64,
    result: AuditResult,
) -> Result<(), LinkError> {
    records
        .append_audit(&AuditEntry {
            actor,
            action: action.to_owned(),
            target: target.to_string(),
            at,
            surface_ref: None,
            result,
        })
        .await?;
    Ok(())
}

/// The token argument of `/ariel link`.
const TOKEN_ARG: ArgSpec = ArgSpec {
    name: "token",
    summary: "The link token an administrator gave you",
    required: true,
};

/// `/ariel link <token>`. Anyone can run it, linked or not, in any channel: it is
/// how an account gets linked in the first place (ADR 0009).
pub const COMMAND: CommandSpec = CommandSpec {
    name: "link",
    summary: "Link this chat account to your fleet identity",
    args: &[TOKEN_ARG],
    min_role: Role::Viewer,
};

/// Answer a `/ariel link` command. The reply is always private and never repeats
/// the token, which would otherwise sit in the chat history.
pub async fn respond(records: &Records, command: &Command, now_ms: i64) {
    let reply = match command.args.get(TOKEN_ARG.name) {
        None => Message::text("Give the token you were issued: `/ariel link <token>`."),
        Some(token) => match redeem(records, token, &command.user, None, now_ms).await {
            Ok(linked) => Message {
                severity: Severity::Success,
                ..Message::text(format!(
                    "Linked. You now hold **{}** {}.",
                    role_name(linked.role),
                    scope_phrase(&linked.scope)
                ))
            },
            Err(error) if error.is_refusal() => Message {
                severity: Severity::Warning,
                ..Message::text(format!("Not linked: {error}."))
            },
            Err(error) => {
                tracing::error!(%error, "account linking failed");
                Message {
                    severity: Severity::Failure,
                    ..Message::text("Linking failed on our side; try again shortly.")
                }
            }
        },
    };
    if let Err(error) = command.responder.reply(&reply, Visibility::Private).await {
        tracing::warn!(%error, "could not answer /ariel link");
    }
}

fn role_name(role: FleetRole) -> &'static str {
    match role {
        FleetRole::Viewer => "viewer",
        FleetRole::Operator => "operator",
        FleetRole::Admin => "admin",
    }
}

fn scope_phrase(scope: &GrantScope) -> String {
    match scope {
        GrantScope::Fleet => "across the fleet".to_owned(),
        GrantScope::Workspace(name) => format!("in workspace `{name}`"),
    }
}
