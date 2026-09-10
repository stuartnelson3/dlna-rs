//! The `ContentSource` extension point: "how the file list is enumerated
//! to the client" (see docs/DESIGN.md). The trait lives in `core` — not in
//! `content`, where its implementations live — so that `core::dispatch`
//! can call through it without `core` depending on any concrete
//! implementation. That dependency direction (implementations depend on
//! core, never the reverse) is what lets `core` stand alone if a
//! downstream consumer ever wants a different organizing scheme without
//! forking anything in `core`.

use crate::index::{Entry, ObjectId};

/// Given a container ID, returns its children; given any ID, returns the
/// entry it names. `id` is always attacker-influenced in the end (it
/// round-trips through a Browse SOAP request — see docs/THREAT_MODEL.md on
/// the ObjectID namespace), so implementations must fail closed: an ID
/// this source doesn't recognize is `None`, never a panic or an
/// out-of-bounds index.
pub trait ContentSource: Send + Sync {
    fn children(&self, id: &ObjectId) -> Option<Vec<Entry>>;
    fn entry(&self, id: &ObjectId) -> Option<Entry>;
}
