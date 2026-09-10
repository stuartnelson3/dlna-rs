//! Request routing: a plain, synchronous match from `(method, path)` to a
//! [`Route`]. Kept as a pure decision separate from the async handler that
//! executes it, so routing logic is unit-testable without a running
//! server. Phases 5 and 6 extend this same match, not a different shape.

use hyper::Method;

use crate::core::device::ServiceType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    DeviceDescription,
    Scpd(ServiceType),
    Control(ServiceType),
    NotFound,
}

pub fn route(method: &Method, path: &str) -> Route {
    match *method {
        Method::GET if path == "/description.xml" => Route::DeviceDescription,
        Method::GET => scpd_route(path),
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
}
