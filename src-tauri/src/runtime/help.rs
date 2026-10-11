//! `web --help` parsing — the capability probe.
//!
//! dshell is a developer-preview-era tool sitting on top of another
//! developer-preview-era tool. Hardcoding flags would break on every DSH
//! release (0.2.0-rc.2 exits 1 on flags that 0.2.1 added), so flags are
//! discovered from the target's own help text and cached per process.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct FlagSupport {
    pub no_open: bool,
    pub port: bool,
    pub host: bool,
    pub trusted_host: bool,
    pub public_url: bool,
    pub tls: bool,
}


impl FlagSupport {
    /// The flag set of every known DSH release (0.2.0-rc.2 through 0.2.1).
    /// Used when `--help` cannot be run at all.
    pub fn assumed() -> Self {
        Self {
            no_open: true,
            port: true,
            host: true,
            trusted_host: true,
            public_url: false,
            tls: false,
        }
    }
}

pub fn parse(help_text: &str) -> FlagSupport {
    let lower = help_text.to_lowercase();
    let has = |flag: &str| lower.contains(flag);
    FlagSupport {
        no_open: has("--no-open"),
        port: has("--port"),
        host: has("--host"),
        trusted_host: has("--trusted-host"),
        public_url: has("--public-url"),
        tls: has("--tls-cert") || has("--tls-key"),
    }
}

fn cache() -> &'static Mutex<HashMap<String, FlagSupport>> {
    static CACHE: OnceLock<Mutex<HashMap<String, FlagSupport>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn cache_get(key: &str) -> Option<FlagSupport> {
    cache().lock().ok()?.get(key).copied()
}

pub fn cache_put(key: String, flags: FlagSupport) {
    if let Ok(mut map) = cache().lock() {
        map.insert(key, flags);
    }
}

pub fn cache_clear() {
    if let Ok(mut map) = cache().lock() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_0_2_0_help() {
        let text = "Usage: dsh web [options]\n\n  --host <host>   bind address\n  --port <port>   listen port\n  --trusted-host <authority...>\n  --no-open       do not open a browser\n  -h, --help      display help\n";
        let flags = parse(text);
        assert!(flags.no_open && flags.port && flags.host && flags.trusted_host);
        assert!(!flags.public_url && !flags.tls);
    }

    #[test]
    fn parses_0_2_1_additional_help() {
        let text = "--public-url <url>\n--tls-cert <file>\n--tls-key <file>\n--no-open\n--port\n";
        let flags = parse(text);
        assert!(flags.public_url && flags.tls && flags.no_open && flags.port);
        assert!(!flags.host);
    }
}
