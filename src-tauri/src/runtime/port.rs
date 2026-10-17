//! The port dshell asks DSH to bind.
//!
//! `--port 0` hands the choice to the OS, and on Windows that means the *dynamic*
//! range — 1024–15000 on many machines, 49152+ on others. A handful of those
//! ports are forbidden to every Chromium-based client (IRC's 6665–6669, X11's
//! 6000, Amanda's 10080, …): the backend serves happily while the tab shows
//! `ERR_UNSAFE_PORT` before a single packet is sent, with no log line anywhere
//! that explains it. That is exactly how a healthy install reads as broken.
//!
//! So dshell picks the port itself: a value from a band outside the common
//! dynamic ranges, never from Chromium's restricted list, verified free by
//! binding it just long enough to learn that nothing else holds it. The value
//! goes on the command line as `--port <n>`; the port actually bound is still
//! read back from the readiness line.

use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// First port of the band dshell picks from.
pub const PICK_START: u16 = 20_000;
/// Last port of the band dshell picks from, inclusive.
pub const PICK_END: u16 = 29_999;
/// Width of the band, as the modulus every probe wraps around.
const PICK_SPAN: u32 = (PICK_END - PICK_START) as u32 + 1;
/// How far to walk before giving up and letting DSH choose for itself.
///
/// The first candidate is free in practice; the cap only stops a pathologically
/// busy machine from turning a launch into a 10,000-syscall bind sweep.
const MAX_ATTEMPTS: u32 = 256;

/// Chromium's `kRestrictedPorts` (`net/base/port_util.cc`), which every WebView2
/// and Chrome/Edge build enforces before the socket is even opened.
///
/// 0 is included because `--port` must never reach DSH as a request to bind
/// "any port": that is how a launch ends up on a forbidden one in the first place.
pub const BROWSER_RESTRICTED_PORTS: &[u16] = &[
    0, 1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101,
    102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427,
    465, 512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990,
    993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 5060, 5061, 6000, 6566, 6665, 6666, 6667, 6668,
    6669, 6697, 10080,
];

/// Is this port one no Chromium-based client will connect to?
pub fn is_browser_restricted_port(port: u16) -> bool {
    BROWSER_RESTRICTED_PORTS.contains(&port)
}

/// A free port from [`PICK_START`]..=[`PICK_END`] that every client will accept.
///
/// `None` means the walk found nothing (or the OS refused every probe); the
/// caller then falls back to `--port 0` and the readiness guard reports the
/// outcome instead of pretending the port was fine.
pub fn pick_listen_port() -> Option<u16> {
    let start = start_offset();
    (0..MAX_ATTEMPTS)
        .map(|step| PICK_START + ((u32::from(start) + step) % PICK_SPAN) as u16)
        .find(|port| !is_browser_restricted_port(*port) && is_free(*port))
}

/// Where this launch starts walking the band.
///
/// Spread out on purpose: two tabs started in the same millisecond must not
/// probe the same candidates in the same order, or the second one pays for the
/// first one's port. A per-process turn counter supplies that separation even
/// when the clock is too coarse to distinguish the calls.
fn start_offset() -> u16 {
    static TURNS: AtomicU16 = AtomicU16::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    let turn = u32::from(TURNS.fetch_add(1, Ordering::Relaxed));
    let mixed = nanos
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12_345)
        .wrapping_add(turn.wrapping_mul(7_919));
    (mixed % PICK_SPAN) as u16
}

/// Can this process bind the port right now?
///
/// The listener is dropped immediately, so the answer is "free a moment ago".
/// DSH binds it microseconds later; if something else wins that race the backend
/// exits with `EADDRINUSE`, which the existing classifier already turns into a
/// retryable "port in use" failure — the same class of failure `--port 0` was
/// introduced to avoid, now with a port that can actually be opened.
fn is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dynamic_range_ports_that_browsers_refuse_are_recognised() {
        // The observed failure: Windows handed DSH port 6666 and the tab died
        // with ERR_UNSAFE_PORT while the server answered 401.
        for port in [6665, 6666, 6667, 6668, 6669, 6000, 6566, 6697, 10080] {
            assert!(is_browser_restricted_port(port), "{port} must be restricted");
        }
        for port in [3000, 43129, 8080, 20_000, 29_999] {
            assert!(!is_browser_restricted_port(port), "{port} must be allowed");
        }
    }

    /// The band is the whole point of not asking the OS: every port in it is
    /// already legal, so the walk never has to skip one for that reason.
    #[test]
    fn the_pick_band_holds_no_restricted_port() {
        let offending: Vec<u16> = (PICK_START..=PICK_END)
            .filter(|port| is_browser_restricted_port(*port))
            .collect();
        assert!(offending.is_empty(), "{offending:?}");
    }

    #[test]
    fn a_picked_port_is_inside_the_band_and_bindable() {
        let port = pick_listen_port().expect("a free port in the band");
        assert!((PICK_START..=PICK_END).contains(&port), "{port}");
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("still free");
        drop(listener);
    }

    /// Two launches must not be handed the same port while the first one holds
    /// it: the walk skips what is already bound.
    #[test]
    fn a_busy_port_is_skipped() {
        let first = pick_listen_port().expect("a free port in the band");
        let held = TcpListener::bind(("127.0.0.1", first)).expect("bind the picked port");
        let second = pick_listen_port().expect("another free port in the band");
        assert_ne!(first, second);
        drop(held);
    }

    /// The probe must not leave a listener behind: a second bind of the same port
    /// succeeding is the proof.
    #[test]
    fn probing_releases_the_port() {
        let port = pick_listen_port().expect("a free port in the band");
        for _ in 0..3 {
            assert!(is_free(port), "{port} was leaked by a probe");
        }
    }
}
