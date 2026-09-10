//! `MetadataProvider` implementations. The trait lives in
//! `core::metadata_provider`, not here — implementations depend on core,
//! never the reverse, same rule as `content` and `transform`.

pub mod filename;
