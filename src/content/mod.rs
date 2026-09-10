//! `ContentSource` implementations: the extension point for "how the file
//! list is enumerated to the client" (see docs/DESIGN.md). The trait
//! definition lives here; implementations live in their own modules and
//! depend on this one, never the reverse.

pub mod folder;

use crate::index::{Entry, ObjectId};

/// Given a container ID, returns its children; given any ID, returns the
/// entry it names. `id` is always attacker-influenced in the end (it
/// round-trips through a Browse SOAP request once Phase 5 lands — see
/// docs/THREAT_MODEL.md on the ObjectID namespace), so implementations
/// must fail closed: an ID this source doesn't recognize is `None`, never
/// a panic or an out-of-bounds index.
pub trait ContentSource: Send + Sync {
    fn children(&self, id: &ObjectId) -> Option<Vec<Entry>>;
    fn entry(&self, id: &ObjectId) -> Option<Entry>;
}
