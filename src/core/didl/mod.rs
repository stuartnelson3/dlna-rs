//! The DIDL-Lite data model and XML generation. Takes `index::Entry`
//! directly rather than defining a parallel `MediaContainer`/`MediaItem`
//! shape — `Entry` already carries everything a `<container>`/`<item>`
//! element needs (id, parent, title, child count or path/size), so a
//! second data shape here would just be mapping boilerplate between two
//! things that mean the same thing.

pub mod format;

use quick_xml::escape::escape;

use crate::index::Entry;

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
                id = c.id,
                count = c.child_count,
                title = escape(&c.title),
            )
        }
        Entry::Item(i) => format!(
            r#"<item id="{id}" parentID="{parent}" restricted="1"><dc:title>{title}</dc:title><upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo="{protocol_info}" size="{size}">{url}</res></item>"#,
            id = i.id,
            parent = i.parent_id,
            title = escape(&i.title),
            protocol_info = format::protocol_info(&i.path),
            size = i.size,
            url = item_url(base_url, &i.id),
        ),
    }
}

fn item_url(base_url: &str, id: &crate::index::ObjectId) -> String {
    format!("{base_url}/item/{id}")
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
}
