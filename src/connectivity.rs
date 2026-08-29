//! Whether this machine can reach the internet.
//!
//! The bar answers one question — can an agent still talk to its provider —
//! so this measures reachability, not bandwidth. A throughput test would cost
//! real traffic and would still read zero on an idle link, which is exactly
//! when the bar is being looked at.

use std::{
    net::{SocketAddr, TcpStream},
    time::Duration,
};

pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// One dropped packet is not an outage. Two consecutive failures are.
const FAILURES_BEFORE_OFFLINE: u8 = 2;

/// Well-known anycast resolver on 443: reachable from every network worth
/// calling online, and it answers without us sending a request.
const PROBE_ADDRESS: [u8; 4] = [1, 1, 1, 1];
const PROBE_PORT: u16 = 443;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Connectivity {
    online: bool,
    consecutive_failures: u8,
}

impl Default for Connectivity {
    fn default() -> Self {
        // Assume online until proven otherwise: a false offline dot on startup
        // would be the bar's loudest signal fired at its least certain moment.
        Self {
            online: true,
            consecutive_failures: 0,
        }
    }
}

impl Connectivity {
    pub(crate) fn is_online(self) -> bool {
        self.online
    }

    /// Folds one probe result in. Returns whether the rendered state changed.
    pub(crate) fn observe(&mut self, reachable: bool) -> bool {
        let previous = self.online;
        if reachable {
            self.consecutive_failures = 0;
            self.online = true;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= FAILURES_BEFORE_OFFLINE {
                self.online = false;
            }
        }
        previous != self.online
    }
}

/// Blocking reachability probe. Callers run it off the render thread.
pub(crate) fn probe() -> bool {
    let address = SocketAddr::from((PROBE_ADDRESS, PROBE_PORT));
    TcpStream::connect_timeout(&address, PROBE_TIMEOUT).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_failure_does_not_declare_an_outage() {
        let mut connectivity = Connectivity::default();
        assert!(!connectivity.observe(false));
        assert!(connectivity.is_online());
    }

    #[test]
    fn two_consecutive_failures_go_offline_and_one_success_returns() {
        let mut connectivity = Connectivity::default();
        connectivity.observe(false);
        assert!(connectivity.observe(false));
        assert!(!connectivity.is_online());
        assert!(connectivity.observe(true));
        assert!(connectivity.is_online());
    }

    #[test]
    fn a_success_between_failures_resets_the_streak() {
        let mut connectivity = Connectivity::default();
        connectivity.observe(false);
        connectivity.observe(true);
        connectivity.observe(false);
        assert!(connectivity.is_online());
    }
}
