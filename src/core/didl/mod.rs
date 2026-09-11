//! The DIDL-Lite data model and XML generation. Takes `index::Entry`
//! directly rather than defining a parallel `MediaContainer`/`MediaItem`
//! shape — `Entry` already carries everything a `<container>`/`<item>`
//! element needs (id, parent, title, child count or path/size), so a
//! second data shape here would just be mapping boilerplate between two
//! things that mean the same thing.

pub mod format;

use quick_xml::escape::escape;

use crate::index::{Entry, Item, ObjectId};

const DIDL_NAMESPACES: &str = r#"xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/""#;

/// Renders a list of entries as a `<DIDL-Lite>` document. Used both for
/// `BrowseDirectChildren` (a container's children) and `BrowseMetadata`
/// (a single-element list: the object itself). `base_url` is the
/// `http://host:port` prefix item resource URLs are built from — the same
/// one `core::http::description` uses for `URLBase`.
pub fn render(entries: &[Entry], base_url: &str) -> String {
    let body: String = entries
        .iter()
        .map(|entry| render_one(entry, base_url))
        .collect();
    format!("<DIDL-Lite {DIDL_NAMESPACES}>{body}</DIDL-Lite>")
}

fn render_one(entry: &Entry, base_url: &str) -> String {
    match entry {
        Entry::Container(c) => {
            let parent = c
                .parent_id
                .as_ref()
                .map_or_else(|| "-1".to_string(), ToString::to_string);
            format!(
                r#"<container id="{id}" parentID="{parent}" restricted="1" childCount="{count}"><dc:title>{title}</dc:title><upnp:class>object.container.storageFolder</upnp:class></container>"#,
                id = escape(c.id.to_string()),
                parent = escape(parent),
                count = c.child_count,
                title = escape(&c.title),
            )
        }
        Entry::Item(i) => format!(
            r#"<item id="{id}" parentID="{parent}" restricted="1"><dc:title>{title}</dc:title>{extras}<upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo="{protocol_info}" size="{size}"{res_attrs}>{url}</res></item>"#,
            id = escape(i.id.to_string()),
            parent = escape(i.parent_id.to_string()),
            title = escape(&i.title),
            extras = extra_item_fields(i, base_url),
            protocol_info = format::protocol_info(&i.path),
            size = i.size,
            res_attrs = res_attributes(i),
            url = item_url(base_url, &i.id),
        ),
    }
}

/// The optional UPnP ContentDirectory:1 `<res>` attributes describing a
/// track's real audio properties - `duration`/`bitrate`/
/// `sampleFrequency`/`bitsPerSample`/`nrAudioChannels`, verified
/// against the actual spec (bitrate is bytes/sec, not bits/sec - a
/// common real-world bug in other implementations). Every one is
/// independently optional, per the spec's own XSD (only `protocolInfo`
/// is required on `<res>`), so this omits whichever ones a
/// `MetadataProvider` couldn't determine rather than guessing - never a
/// fabricated `0`. All five values are plain unsigned integers, never
/// escaped: none can contain an XML-special character.
fn res_attributes(item: &Item) -> String {
    let mut attrs = String::new();
    if let Some(millis) = item.duration_millis {
        attrs.push_str(&format!(r#" duration="{}""#, format_duration(millis)));
    }
    if let Some(bitrate) = item.bitrate {
        attrs.push_str(&format!(r#" bitrate="{bitrate}""#));
    }
    if let Some(rate) = item.sample_rate {
        attrs.push_str(&format!(r#" sampleFrequency="{rate}""#));
    }
    if let Some(bits) = item.bits_per_sample {
        attrs.push_str(&format!(r#" bitsPerSample="{bits}""#));
    }
    if let Some(channels) = item.channels {
        attrs.push_str(&format!(r#" nrAudioChannels="{channels}""#));
    }
    attrs
}

/// Renders `total_millis` as the spec's `res@duration` grammar,
/// `H*:MM:SS.F*`: an unpadded hour count (may be `0`), always
/// zero-padded minutes/seconds, always three fractional digits.
fn format_duration(total_millis: u64) -> String {
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis % 3_600_000) / 60_000;
    let seconds = (total_millis % 60_000) / 1000;
    let millis = total_millis % 1000;
    format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// The real-tag elements a track's `MetadataProvider` may or may not
/// have filled in (see `core::metadata_provider`). Each is left out
/// entirely when its source field is `None`/`false` — an absent tag
/// means no element, never an empty one.
fn extra_item_fields(item: &Item, base_url: &str) -> String {
    let mut fields = String::new();

    // Both dc:creator and upnp:artist, deliberately: real DLNA clients
    // disagree on which one they read for the artist, so emitting both
    // costs nothing and works with more of them.
    if let Some(artist) = &item.artist {
        let escaped = escape(artist);
        fields.push_str(&format!(
            "<dc:creator>{escaped}</dc:creator><upnp:artist>{escaped}</upnp:artist>"
        ));
    }
    if let Some(album) = &item.album {
        fields.push_str(&format!("<upnp:album>{}</upnp:album>", escape(album)));
    }
    if let Some(genre) = &item.genre {
        fields.push_str(&format!("<upnp:genre>{}</upnp:genre>", escape(genre)));
    }
    if item.has_art {
        // No dlna:profileID attribute: this server serves the embedded
        // picture byte-for-byte, with no resizing, so a profile claim
        // like JPEG_TN (a specific pixel size) would be a promise it
        // can't back. Omitting it also means no new XML namespace is
        // needed here at all.
        fields.push_str(&format!(
            "<upnp:albumArtURI>{}</upnp:albumArtURI>",
            art_url(base_url, &item.id)
        ));
    }

    fields
}

fn item_url(base_url: &str, id: &ObjectId) -> String {
    format!("{base_url}/item/{id}")
}

fn art_url(base_url: &str, id: &ObjectId) -> String {
    format!("{base_url}/art/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuilder;
    use quick_xml::Reader;
    use quick_xml::events::Event;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn assert_well_formed(xml: &str) {
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(err) => panic!("malformed XML at {}: {err}", reader.buffer_position()),
            }
            buf.clear();
        }
    }

    #[test]
    fn renders_a_well_formed_empty_didl_lite_document() {
        assert_well_formed(&render(&[], "http://192.168.1.5:8200"));
    }

    #[test]
    fn renders_a_container() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_container(&crate::index::ObjectId::root(), "Music".to_string());
        let index = builder.build();
        let entry = index.entry(&id).unwrap();

        let xml = render(&[entry], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        assert!(xml.contains(&format!(r#"<container id="{id}" parentID="0""#)));
        assert!(xml.contains("<dc:title>Music</dc:title>"));
        assert!(xml.contains("<upnp:class>object.container.storageFolder</upnp:class>"));
        assert!(xml.contains(r#"childCount="0""#));
    }

    #[test]
    fn root_containers_own_metadata_uses_parent_id_negative_one() {
        let index = IndexBuilder::new().build();
        let root = index.entry(&crate::index::ObjectId::root()).unwrap();
        let xml = render(&[root], "http://192.168.1.5:8200");
        assert!(xml.contains(r#"parentID="-1""#));
    }

    #[test]
    fn renders_an_item_with_correct_protocol_info_and_url() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_item(
            &crate::index::ObjectId::root(),
            "Track.mp3".to_string(),
            PathBuf::from("/music/Track.mp3"),
            12345,
            SystemTime::UNIX_EPOCH,
        );
        let index = builder.build();
        let entry = index.entry(&id).unwrap();

        let xml = render(&[entry], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        assert!(xml.contains("<dc:title>Track.mp3</dc:title>"));
        assert!(xml.contains("<upnp:class>object.item.audioItem.musicTrack</upnp:class>"));
        assert!(xml.contains(r#"protocolInfo="http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000""#));
        assert!(xml.contains(r#"size="12345""#));
        assert!(xml.contains(&format!(">http://192.168.1.5:8200/item/{id}<")));
    }

    #[test]
    fn escapes_special_characters_in_titles() {
        let mut builder = IndexBuilder::new();
        let id = builder.add_container(
            &crate::index::ObjectId::root(),
            "Rock & Roll <Live>".to_string(),
        );
        let index = builder.build();
        let entry = index.entry(&id).unwrap();

        let xml = render(&[entry], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        assert!(!xml.contains("<Live>"), "raw '<' should have been escaped");
    }

    /// Real regression: `content::music_library`'s tag-derived synthetic
    /// artist IDs are built from the tag's own text (e.g.
    /// `tag-artist:art blakey & the jazz messengers`), unlike every
    /// other ID in this project, which is always a plain digit string.
    /// An unescaped `&` in an XML attribute value makes the whole
    /// document unparsable - not just that one entry - which is exactly
    /// what made a real DLNA client report the entire Artists listing
    /// as empty, confirmed against a real running instance.
    #[test]
    fn a_special_character_in_a_container_id_does_not_break_the_whole_document() {
        let container = crate::index::Container {
            id: crate::index::ObjectId::new("tag-artist:art blakey & the jazz messengers"),
            parent_id: Some(crate::index::ObjectId::root()),
            title: "Art Blakey & The Jazz Messengers".to_string(),
            child_count: 2,
        };
        let xml = render(&[Entry::Container(container)], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
    }

    #[test]
    fn a_special_character_in_an_items_parent_id_does_not_break_the_whole_document() {
        let item = Item {
            id: crate::index::ObjectId::new("42"),
            parent_id: crate::index::ObjectId::new("tag-artist:ac/dc"),
            title: "Track".to_string(),
            path: PathBuf::from("/music/Track.mp3"),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            artist: None,
            album: None,
            genre: None,
            has_art: false,
            duration_millis: None,
            bitrate: None,
            sample_rate: None,
            bits_per_sample: None,
            channels: None,
        };
        let xml = render(&[Entry::Item(item)], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
    }

    fn item_with_tags(tags: crate::index::TrackTags) -> Entry {
        let mut builder = IndexBuilder::new();
        let id = builder.add_item_with_tags(
            &crate::index::ObjectId::root(),
            "Track.mp3".to_string(),
            PathBuf::from("/music/Track.mp3"),
            1,
            SystemTime::UNIX_EPOCH,
            tags,
        );
        builder.build().entry(&id).unwrap()
    }

    #[test]
    fn renders_real_tags_and_cover_art_when_present() {
        let entry = item_with_tags(crate::index::TrackTags {
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            genre: Some("Test Genre".to_string()),
            has_art: true,
            duration_millis: Some(3_661_500), // 1:01:01.500
            bitrate: Some(16000),
            sample_rate: Some(44100),
            bits_per_sample: Some(16),
            channels: Some(2),
        });

        let xml = render(std::slice::from_ref(&entry), "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        assert!(xml.contains("<dc:creator>Test Artist</dc:creator>"));
        assert!(xml.contains("<upnp:artist>Test Artist</upnp:artist>"));
        assert!(xml.contains("<upnp:album>Test Album</upnp:album>"));
        assert!(xml.contains("<upnp:genre>Test Genre</upnp:genre>"));
        assert!(xml.contains(&format!(
            "<upnp:albumArtURI>http://192.168.1.5:8200/art/{}</upnp:albumArtURI>",
            entry.id()
        )));
        assert!(xml.contains(r#"duration="1:01:01.500""#));
        assert!(xml.contains(r#"bitrate="16000""#));
        assert!(xml.contains(r#"sampleFrequency="44100""#));
        assert!(xml.contains(r#"bitsPerSample="16""#));
        assert!(xml.contains(r#"nrAudioChannels="2""#));
    }

    #[test]
    fn omits_every_extra_element_when_no_tags_are_present() {
        let entry = item_with_tags(crate::index::TrackTags::default());

        let xml = render(&[entry], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        for element in [
            "dc:creator",
            "upnp:artist",
            "upnp:album",
            "upnp:genre",
            "upnp:albumArtURI",
        ] {
            assert!(!xml.contains(element), "should not render <{element}>");
        }
        for attribute in [
            "duration=",
            "bitrate=",
            "sampleFrequency=",
            "bitsPerSample=",
            "nrAudioChannels=",
        ] {
            assert!(!xml.contains(attribute), "should not render {attribute}");
        }
    }

    #[test]
    fn format_duration_renders_the_hours_digit() {
        assert_eq!(format_duration(3_661_500), "1:01:01.500");
        assert_eq!(format_duration(500), "0:00:00.500");
        assert_eq!(format_duration(0), "0:00:00.000");
    }

    #[test]
    fn escapes_special_characters_in_tag_fields() {
        let entry = item_with_tags(crate::index::TrackTags {
            artist: Some("Rock & Roll <Live>".to_string()),
            ..Default::default()
        });

        let xml = render(&[entry], "http://192.168.1.5:8200");
        assert_well_formed(&xml);
        assert!(!xml.contains("<Live>"), "raw '<' should have been escaped");
    }
}
