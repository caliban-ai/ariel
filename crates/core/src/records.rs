//! The gonzalo client for Ariel's access-control records (ADR 0003).
//!
//! People, identity bindings, role grants, channel configuration, link tokens and
//! audit entries are gonzalo records whose kinds and keys gonzalo defines
//! (gonzalo ADR 0022). [`Records`] reads and writes them as typed values over any
//! gonzalo [`Store`]: `ServerStore` talking to gonzalod in production, `FsStore`
//! in tests.
//!
//! Writes are optimistic. A write that loses a race returns
//! [`Write::Conflict`] carrying the record that won, as an ordinary result the
//! caller decides how to handle, not an error.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub use gonzalo;

use gonzalo::fleet::{
    AUDIT_ENTRIES_COLLECTION, CHANNELS_COLLECTION, FLEET_AUDIT_NAMESPACE, FLEET_NAMESPACE,
    IDENTITY_BINDINGS_COLLECTION, LINK_TOKENS_COLLECTION, PEOPLE_COLLECTION,
    ROLE_GRANTS_COLLECTION,
};
use gonzalo::{
    AuditEntry, ChannelConfig, CoreError, DeleteResult, FleetKeyError, Identity, IdentityBinding,
    KeyPrefix, LinkToken, Meta, Person, PutResult, Record, RecordCodec, RecordKey, RecordKind,
    Revision, RoleGrant, ServerStore, Store,
};

/// How many nonces [`Records::append_audit`] tries before giving up. A collision
/// needs two entries in the same millisecond with the same 128-bit nonce, so a
/// retry at all means something is wrong with the random source.
const AUDIT_NONCE_ATTEMPTS: u32 = 3;

/// The `origin_system` stamped on every record Ariel writes.
const ORIGIN_SYSTEM: &str = "ariel";

/// A record kind Ariel stores, with where gonzalo keeps it.
pub trait FleetRecord: RecordCodec + Send + Sync + 'static {
    const KIND: RecordKind;
    const NAMESPACE: &'static str;
    const COLLECTION: &'static str;
}

macro_rules! fleet_record {
    ($ty:ty, $namespace:expr, $collection:expr) => {
        impl FleetRecord for $ty {
            const KIND: RecordKind = <$ty>::KIND;
            const NAMESPACE: &'static str = $namespace;
            const COLLECTION: &'static str = $collection;
        }
    };
}

fleet_record!(Person, FLEET_NAMESPACE, PEOPLE_COLLECTION);
fleet_record!(
    IdentityBinding,
    FLEET_NAMESPACE,
    IDENTITY_BINDINGS_COLLECTION
);
fleet_record!(RoleGrant, FLEET_NAMESPACE, ROLE_GRANTS_COLLECTION);
fleet_record!(ChannelConfig, FLEET_NAMESPACE, CHANNELS_COLLECTION);
fleet_record!(LinkToken, FLEET_NAMESPACE, LINK_TOKENS_COLLECTION);
fleet_record!(AuditEntry, FLEET_AUDIT_NAMESPACE, AUDIT_ENTRIES_COLLECTION);

/// A decoded record at a known revision. Pass it back to [`Records::update`] or
/// [`Records::delete`] so the write only applies if nobody changed it since.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    pub key: RecordKey,
    pub revision: Revision,
    pub value: T,
    created: i64,
}

/// The outcome of a create or update.
#[derive(Debug, Clone, PartialEq)]
pub enum Write<T> {
    /// The write applied; this is the record as now stored.
    Committed(Versioned<T>),
    /// Someone else wrote first. Carries the current record, or `None` when the
    /// current record is a deletion marker.
    Conflict(Option<Versioned<T>>),
}

/// The outcome of a delete.
#[derive(Debug, Clone, PartialEq)]
pub enum Delete<T> {
    Deleted,
    /// The record changed since it was read. Carries the current record, or
    /// `None` when it is already deleted.
    Conflict(Option<Versioned<T>>),
}

#[derive(Debug, thiserror::Error)]
pub enum RecordsError {
    /// gonzalod or the store failed, or a body did not decode.
    #[error("gonzalo: {0}")]
    Store(#[from] CoreError),
    #[error("invalid record key: {0}")]
    Key(#[from] FleetKeyError),
    #[error("{key} holds a {found:?} record, not {expected:?}")]
    WrongKind {
        key: RecordKey,
        expected: RecordKind,
        found: RecordKind,
    },
    #[error("no randomness for an audit nonce: {0}")]
    Random(getrandom::Error),
    #[error("audit entry key still collided after {0} nonces")]
    AuditNonces(u32),
}

pub type Result<T> = std::result::Result<T, RecordsError>;

/// Typed access to Ariel's records in one gonzalo store.
#[derive(Clone)]
pub struct Records {
    store: Arc<dyn Store>,
    author: Identity,
}

impl std::fmt::Debug for Records {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Records")
            .field("author", &self.author)
            .finish_non_exhaustive()
    }
}

impl Records {
    /// Records in `store`, written as `author`.
    pub fn new(store: Arc<dyn Store>, author: Identity) -> Self {
        Self { store, author }
    }

    /// Records in gonzalod at `base_url`, authenticated with a bearer `token`
    /// (gonzalo ADR 0015). Nothing is sent until the first read or write.
    pub fn connect(base_url: &str, token: impl Into<String>, author: Identity) -> Result<Self> {
        let store = ServerStore::http_with_token(base_url, token)?;
        Ok(Self::new(Arc::new(store), author))
    }

    /// The record at `key`, or `None` if it does not exist or was deleted.
    pub async fn get<T: FleetRecord>(&self, key: &RecordKey) -> Result<Option<Versioned<T>>> {
        match self.store.get(key).await? {
            Some(record) if !record.is_tombstone() => decode(record).map(Some),
            _ => Ok(None),
        }
    }

    /// The keys of every stored record of kind `T`.
    pub async fn list<T: FleetRecord>(&self) -> Result<Vec<RecordKey>> {
        let prefix = KeyPrefix {
            namespace: Some(T::NAMESPACE.to_owned()),
            collection: Some(T::COLLECTION.to_owned()),
        };
        Ok(self.store.list(&prefix).await?)
    }

    /// Stores `value` at `key`, which must not already hold a record.
    pub async fn create<T: FleetRecord>(&self, key: RecordKey, value: T) -> Result<Write<T>> {
        let now = now_ms();
        let body = value.to_body()?;
        let record = Record {
            key: key.clone(),
            kind: T::KIND,
            revision: Revision::initial(body.bytes()),
            parent: None,
            body,
            meta: self.meta(now, now),
            links: Vec::new(),
            ancestors: Vec::new(),
            deleted_at: None,
        };
        let outcome = self.store.put(record, None).await?;
        written(key, value, now, outcome)
    }

    /// Replaces the record `current` was read from with `value`, unless it has
    /// changed since.
    pub async fn update<T: FleetRecord>(
        &self,
        current: &Versioned<T>,
        value: T,
    ) -> Result<Write<T>> {
        let body = value.to_body()?;
        let record = Record {
            key: current.key.clone(),
            kind: T::KIND,
            revision: current.revision.next(body.bytes()),
            parent: Some(current.revision.clone()),
            body,
            meta: self.meta(current.created, now_ms()),
            links: Vec::new(),
            ancestors: Vec::new(),
            deleted_at: None,
        };
        let outcome = self
            .store
            .put(record, Some(current.revision.clone()))
            .await?;
        written(current.key.clone(), value, current.created, outcome)
    }

    /// Deletes the record `current` was read from, unless it has changed since.
    pub async fn delete<T: FleetRecord>(&self, current: &Versioned<T>) -> Result<Delete<T>> {
        let outcome = self
            .store
            .delete_as(
                &current.key,
                Some(current.revision.clone()),
                Some(self.author.clone()),
            )
            .await?;
        match outcome {
            DeleteResult::Deleted => Ok(Delete::Deleted),
            DeleteResult::Conflict(conflict) => Ok(Delete::Conflict(live(conflict.current)?)),
        }
    }

    /// Appends `entry` to the audit trail under a fresh random key, and returns
    /// that key. Audit entries are written once and never updated.
    pub async fn append_audit(&self, entry: &AuditEntry) -> Result<RecordKey> {
        self.append_audit_with(entry, random_nonce).await
    }

    async fn append_audit_with(
        &self,
        entry: &AuditEntry,
        mut nonce: impl FnMut() -> Result<String>,
    ) -> Result<RecordKey> {
        for _ in 0..AUDIT_NONCE_ATTEMPTS {
            let key = entry.key(&nonce()?)?;
            if let Write::Committed(_) = self.create(key.clone(), entry.clone()).await? {
                return Ok(key);
            }
        }
        Err(RecordsError::AuditNonces(AUDIT_NONCE_ATTEMPTS))
    }

    fn meta(&self, created: i64, updated: i64) -> Meta {
        Meta {
            author: self.author.clone(),
            origin_system: ORIGIN_SYSTEM.to_owned(),
            created,
            updated,
            labels: Default::default(),
        }
    }
}

fn written<T: FleetRecord>(
    key: RecordKey,
    value: T,
    created: i64,
    outcome: PutResult,
) -> Result<Write<T>> {
    match outcome {
        PutResult::Committed(revision) => Ok(Write::Committed(Versioned {
            key,
            revision,
            value,
            created,
        })),
        PutResult::Conflict(conflict) => Ok(Write::Conflict(live(conflict.current)?)),
    }
}

/// `record` decoded, or `None` if it is a deletion marker.
fn live<T: FleetRecord>(record: Record) -> Result<Option<Versioned<T>>> {
    if record.is_tombstone() {
        return Ok(None);
    }
    decode(record).map(Some)
}

fn decode<T: FleetRecord>(record: Record) -> Result<Versioned<T>> {
    if record.kind != T::KIND {
        return Err(RecordsError::WrongKind {
            key: record.key,
            expected: T::KIND,
            found: record.kind,
        });
    }
    Ok(Versioned {
        value: T::from_body(&record.body)?,
        key: record.key,
        revision: record.revision,
        created: record.meta.created,
    })
}

/// 128 random bits as 32 hex characters, within gonzalo's nonce alphabet.
fn random_nonce() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(RecordsError::Random)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gonzalo::{AuditResult, FleetActor, FsStore};

    fn entry() -> AuditEntry {
        AuditEntry {
            actor: FleetActor::Service("ariel-test".into()),
            action: "link".into(),
            target: "p1".into(),
            at: 1_760_000_000_000,
            surface_ref: None,
            result: AuditResult::Succeeded,
        }
    }

    fn records() -> (tempfile::TempDir, Records) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FsStore::new(dir.path()));
        (dir, Records::new(store, Identity::new("ariel-test")))
    }

    #[tokio::test]
    async fn a_colliding_audit_nonce_is_retried_with_a_new_one() {
        let (_dir, records) = records();
        let first = records
            .append_audit_with(&entry(), || Ok("same".into()))
            .await
            .unwrap();

        let mut nonces = ["same", "fresh"].into_iter();
        let second = records
            .append_audit_with(&entry(), || Ok(nonces.next().unwrap().into()))
            .await
            .unwrap();
        assert_eq!(second, entry().key("fresh").unwrap());
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn an_audit_nonce_that_always_collides_gives_up() {
        let (_dir, records) = records();
        let _ = records
            .append_audit_with(&entry(), || Ok("same".into()))
            .await
            .unwrap();

        let err = records
            .append_audit_with(&entry(), || Ok("same".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, RecordsError::AuditNonces(3)), "{err}");
    }

    #[test]
    fn random_nonces_are_32_hex_characters_and_differ() {
        let a = random_nonce().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, random_nonce().unwrap());
    }
}
