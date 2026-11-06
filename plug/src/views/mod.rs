pub(crate) mod clients;
pub(crate) mod overview;
pub(crate) mod servers;
pub(crate) mod tools;

use crate::runtime::DaemonQueryAvailability;

/// Where a status answer came from. Without a daemon it is the config.
pub(crate) fn status_source(availability: DaemonQueryAvailability) -> &'static str {
    match availability {
        DaemonQueryAvailability::Unreachable => "config_only",
        reachable => reachable.status_source(),
    }
}
