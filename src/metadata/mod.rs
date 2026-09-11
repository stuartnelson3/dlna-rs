//! `MetadataProvider` implementations. The trait lives in
//! `core::metadata_provider`, not here — implementations depend on core,
//! never the reverse, same rule as `content` and `transform`.

mod cover_files;
pub(crate) mod filename;
pub(crate) mod tag_cache;
pub mod tags;
