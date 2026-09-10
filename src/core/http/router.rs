//! Request routing: a plain, synchronous match from `(method, path)` to a
//! [`Route`]. Kept as a pure decision separate from the async handler that
//! executes it, so routing logic is unit-testable without a running
//! server. Phases 5 and 6 extend this same match, not a different shape.

use hyper::Method;

use crate::core::device::ServiceType;
use crate::index::ObjectId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    DeviceDescription,
    Scpd(ServiceType),
    Control(ServiceType),
    Item(ObjectId),
    Art(ObjectId),
    NotFound,
}

pub fn route(method: &Method, path: &str) -> Route {
    match *method {
        Method::GET if path == "/description.xml" => Route::DeviceDescription,
        Method::GET => item_route(path)
            .or_else(|| art_route(path))
            .unwrap_or_else(|| scpd_route(path)),
        // No Range/partial-content story for art, so no need to route it
        // for HEAD - a deliberate, small scope call, not an oversight.
        Method::HEAD => item_route(path).unwrap_or(Route::NotFound),
        Method::POST => control_route(path),
        _ => Route::NotFound,
    }
}

fn scpd_route(path: &str) -> Route {
    ServiceType::ALL
        .into_iter()
        .find(|service| path == service.scpd_path())
        .map_or(Route::NotFound, Route::Scpd)
}

fn control_route(path: &str) -> Route {
    ServiceType::ALL
        .into_iter()
        .find(|service| path == service.control_path())
        .map_or(Route::NotFound, Route::Control)
}

fn item_route(path: &str) -> Option<Route> {
    parse_item_path(path).map(|id| Route::Item(ObjectId::new(id)))
}

/// Parses `/item/{id}` request paths. Pure and total (never panics on any
/// input) — this is the one place raw, attacker-controlled request-path
/// text gets turned into something used for a lookup, so it's fuzzed
/// (`fuzz/fuzz_targets/path_resolve.rs`) even though, per
/// docs/THREAT_MODEL.md, our opaque-`ObjectId` URL scheme means the
/// result never becomes a filesystem path directly — only ever an index
/// lookup key, which fails closed on anything that doesn't match a real
/// entry.
pub fn parse_item_path(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/item/")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(id)
}

fn art_route(path: &str) -> Option<Route> {
    parse_art_path(path).map(|id| Route::Art(ObjectId::new(id)))
}

/// Parses `/art/{id}` request paths — same rule as `parse_item_path`,
/// copied rather than shared: two small, single-purpose, pure functions
/// are simpler than one parameterized over a prefix for a two-site
/// reuse (matches this module's existing `item_route`/`scpd_route`
/// shape, which is already two sibling functions, not one).
pub fn parse_art_path(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/art/")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_description() {
        assert_eq!(
            route(&Method::GET, "/description.xml"),
            Route::DeviceDescription
        );
    }

    #[test]
    fn routes_each_service_scpd() {
        for service in ServiceType::ALL {
            assert_eq!(
                route(&Method::GET, &service.scpd_path()),
                Route::Scpd(service)
            );
        }
    }

    #[test]
    fn unknown_path_is_not_found() {
        assert_eq!(route(&Method::GET, "/nope"), Route::NotFound);
    }

    #[test]
    fn wrong_method_is_not_found() {
        assert_eq!(route(&Method::POST, "/description.xml"), Route::NotFound);
    }

    #[test]
    fn routes_each_service_control_endpoint() {
        for service in ServiceType::ALL {
            assert_eq!(
                route(&Method::POST, &service.control_path()),
                Route::Control(service)
            );
        }
    }

    #[test]
    fn get_on_a_control_path_is_not_found() {
        for service in ServiceType::ALL {
            assert_eq!(
                route(&Method::GET, &service.control_path()),
                Route::NotFound
            );
        }
    }

    #[test]
    fn routes_get_and_head_on_an_item_path() {
        assert_eq!(
            route(&Method::GET, "/item/42"),
            Route::Item(ObjectId::new("42"))
        );
        assert_eq!(
            route(&Method::HEAD, "/item/42"),
            Route::Item(ObjectId::new("42"))
        );
    }

    #[test]
    fn post_on_an_item_path_is_not_found() {
        assert_eq!(route(&Method::POST, "/item/42"), Route::NotFound);
    }

    #[test]
    fn parse_item_path_examples() {
        assert_eq!(parse_item_path("/item/42"), Some("42"));
        assert_eq!(parse_item_path("/item/"), None);
        assert_eq!(parse_item_path("/item"), None);
        assert_eq!(parse_item_path("/item/42/"), None);
        assert_eq!(parse_item_path("/item/42/extra"), None);
        assert_eq!(parse_item_path("/other/42"), None);
        assert_eq!(parse_item_path(""), None);
    }

    #[test]
    fn parse_item_path_never_panics_on_arbitrary_short_inputs() {
        let candidates = [
            "",
            "/",
            "/item",
            "/item/",
            "//item//",
            "/item/\u{0}",
            "/item/../../etc/passwd",
        ];
        for candidate in candidates {
            let _ = parse_item_path(candidate);
        }
    }

    #[test]
    fn routes_get_on_an_art_path() {
        assert_eq!(
            route(&Method::GET, "/art/42"),
            Route::Art(ObjectId::new("42"))
        );
    }

    #[test]
    fn head_on_an_art_path_is_not_found() {
        // No Range story for art - a deliberate scope call, not a bug.
        assert_eq!(route(&Method::HEAD, "/art/42"), Route::NotFound);
    }

    #[test]
    fn post_on_an_art_path_is_not_found() {
        assert_eq!(route(&Method::POST, "/art/42"), Route::NotFound);
    }

    #[test]
    fn parse_art_path_examples() {
        assert_eq!(parse_art_path("/art/42"), Some("42"));
        assert_eq!(parse_art_path("/art/"), None);
        assert_eq!(parse_art_path("/art"), None);
        assert_eq!(parse_art_path("/art/42/"), None);
        assert_eq!(parse_art_path("/other/42"), None);
    }

    #[test]
    fn parse_art_path_never_panics_on_arbitrary_short_inputs() {
        let candidates = [
            "",
            "/",
            "/art",
            "/art/",
            "//art//",
            "/art/\u{0}",
            "/art/../../etc/passwd",
        ];
        for candidate in candidates {
            let _ = parse_art_path(candidate);
        }
    }
}
