//! Allowlist доменов: те же Domain Bundles, что и в клиенте NETFORY.
//!
//! Формат `bundles.json` (общий с клиентом; серверу важны только `hosts`):
//! ```json
//! { "version": 1,
//!   "bundles": {
//!     "youtube": { "hosts": ["youtube.com", "*.youtube.com", "*.googlevideo.com"], "mode": "auto" }
//!   } }
//! ```
//! Правило `*.example.com` матчит поддомены любой глубины (но не сам `example.com`),
//! `example.com` — только точное совпадение. Сравнение регистронезависимое, без точки в конце.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Bundle {
    pub hosts: Vec<String>,
    /// Режим клиента (auto | relay | direct) — сервером игнорируется, хранится для совместимости.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Bundles {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub bundles: BTreeMap<String, Bundle>,
}

impl Bundles {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Стартовый набор для тестов MVP: youtube / x / telegram + iptest (2ip.ru).
    pub fn example() -> Self {
        let mut bundles = BTreeMap::new();
        let mk = |hosts: &[&str]| Bundle {
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            mode: Some("auto".into()),
        };
        bundles.insert(
            "youtube".into(),
            mk(&["youtube.com", "*.youtube.com", "youtu.be", "*.googlevideo.com", "*.ytimg.com", "*.ggpht.com", "*.youtube-nocookie.com"]),
        );
        bundles.insert(
            "x".into(),
            mk(&["x.com", "*.x.com", "twitter.com", "*.twitter.com", "*.twimg.com", "t.co"]),
        );
        bundles.insert(
            "telegram".into(),
            mk(&["telegram.org", "*.telegram.org", "t.me", "*.t.me", "*.tdesktop.com", "telegram.me"]),
        );
        // тестовая группа: сервисы «мой IP» — клиент видит IP выхода узла
        bundles.insert("iptest".into(), mk(&["2ip.ru", "*.2ip.ru", "2ip.io", "*.2ip.io"]));
        Bundles { version: 2, bundles }
    }

    /// Имя bundle, которому принадлежит host (или None — запрещено).
    pub fn match_host(&self, host: &str) -> Option<&str> {
        let h = normalize(host);
        if h.is_empty() {
            return None;
        }
        for (name, b) in &self.bundles {
            for rule in &b.hosts {
                if rule_matches(&normalize(rule), &h) {
                    return Some(name.as_str());
                }
            }
        }
        None
    }

    pub fn names(&self) -> Vec<String> {
        self.bundles.keys().cloned().collect()
    }
}

fn normalize(h: &str) -> String {
    h.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn rule_matches(rule: &str, host: &str) -> bool {
    if let Some(suffix) = rule.strip_prefix("*.") {
        // поддомен любой глубины: "a.b.example.com" ✓, "example.com" ✗
        host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
    } else {
        rule == host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_and_exact() {
        let b = Bundles::example();
        assert_eq!(b.match_host("youtube.com"), Some("youtube"));
        assert_eq!(b.match_host("WWW.YouTube.com."), Some("youtube"));
        assert_eq!(b.match_host("rr3---sn-abc.googlevideo.com"), Some("youtube"));
        assert_eq!(b.match_host("googlevideo.com"), None);
        assert_eq!(b.match_host("notyoutube.com"), None);
        assert_eq!(b.match_host("x.com"), Some("x"));
        assert_eq!(b.match_host("example.org"), None);
    }
}
