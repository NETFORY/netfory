//! Директория релеев (NIP-1 §6): подписанные описатели по gossip-топику `mycelium/dir/1`
//! + вшитый bootstrap-список. Политика выхода и лимиты релея — здесь же.
use anyhow::Result;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DIR_TOPIC: &[u8] = b"netfory/mycelium/dir/1";
pub const PROTO_VERSION: u16 = 1;
/// Описатель старше этого — релей считается ушедшим.
pub const DESCRIPTOR_TTL_SECS: u64 = 30 * 60;
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayDescriptor {
    pub node: EndpointId,
    pub version: u16,
    pub guard: bool,
    pub middle: bool,
    pub exit: bool,
    /// Лимит канала, байт/с (0 — без лимита).
    pub bandwidth: u64,
    pub max_circuits: u32,
    /// Разрешённые ALPN-префиксы Exit.
    pub exit_alpns: Vec<String>,
    /// «Семья» релеев одного оператора — два хопа из одной семьи запрещены.
    pub family: String,
    pub ts: u64,
    #[serde(with = "sig_hex")]
    pub sig: [u8; 64],
    /// Подсказки адреса (ip:port, url реле iroh) — вне подписи: ложная подсказка лишь не даст
    /// соединиться (iroh всё равно проверяет ключ узла), старые узлы поле игнорируют.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
}

/// Подсказки адреса → `EndpointAddr` (прямые ip:port и реле iroh).
pub fn endpoint_addr(node: EndpointId, addrs: &[String]) -> iroh::EndpointAddr {
    let mut a = iroh::EndpointAddr::new(node);
    for s in addrs {
        if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
            a = a.with_ip_addr(sa);
        } else if let Ok(u) = s.parse::<iroh::RelayUrl>() {
            a = a.with_relay_url(u);
        }
    }
    a
}

/// Сбой релея «забывается» через это время — узел снова получает полный вес.
pub const FAILURE_FORGET: Duration = Duration::from_secs(5 * 60);

mod sig_hex {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        hex::encode(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let b = hex::decode(s).map_err(serde::de::Error::custom)?;
        b.try_into().map_err(|_| serde::de::Error::custom("sig len"))
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl RelayDescriptor {
    fn signable(&self) -> Vec<u8> {
        serde_json::json!({
            "node": self.node.to_string(), "v": self.version, "g": self.guard, "m": self.middle, "e": self.exit,
            "bw": self.bandwidth, "mc": self.max_circuits, "alpns": self.exit_alpns, "fam": self.family, "ts": self.ts,
        })
        .to_string()
        .into_bytes()
    }
    pub fn sign(mut self, secret: &iroh::SecretKey) -> Self {
        self.sig = secret.sign(&self.signable()).to_bytes();
        self
    }
    pub fn verify(&self) -> bool {
        self.version == PROTO_VERSION && self.node.verify(&self.signable(), &iroh::Signature::from_bytes(&self.sig)).is_ok()
    }
    pub fn fresh(&self) -> bool {
        now_secs().saturating_sub(self.ts) < DESCRIPTOR_TTL_SECS
    }
}

/// Описатель скрытого приёма (NIP-1 §7): владелец и его intro-точки, подписан ключом владельца.
/// Публикуется в тот же gossip-топик, что и описатели релеев; TTL как у релеев.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    pub owner: EndpointId,
    pub intros: Vec<EndpointId>,
    pub ts: u64,
    #[serde(with = "sig_hex")]
    pub sig: [u8; 64],
}

impl ServiceDescriptor {
    fn signable(&self) -> Vec<u8> {
        let intros: Vec<String> = self.intros.iter().map(|i| i.to_string()).collect();
        serde_json::json!({ "svc": self.owner.to_string(), "intros": intros, "ts": self.ts }).to_string().into_bytes()
    }
    pub fn sign(mut self, secret: &iroh::SecretKey) -> Self {
        self.sig = secret.sign(&self.signable()).to_bytes();
        self
    }
    pub fn verify(&self) -> bool {
        !self.intros.is_empty() && self.intros.len() <= 4 && self.owner.verify(&self.signable(), &iroh::Signature::from_bytes(&self.sig)).is_ok()
    }
    pub fn fresh(&self) -> bool {
        now_secs().saturating_sub(self.ts) < DESCRIPTOR_TTL_SECS
    }
}

/// Политика выхода: Exit пропускает только Netfory-протоколы (защита операторов, NIP-1 §6.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitPolicy {
    pub allowed_alpn_prefixes: Vec<String>,
}

impl Default for ExitPolicy {
    fn default() -> Self {
        Self {
            allowed_alpn_prefixes: ["netfory/", "sth/", "/iroh-bytes/", "iroh-gossip/", "ntfry/"].iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ExitPolicy {
    pub fn allows(&self, alpn: &[u8]) -> bool {
        let a = String::from_utf8_lossy(alpn);
        self.allowed_alpn_prefixes.iter().any(|p| a.starts_with(p.as_str()))
    }
}

/// Лимиты релея (NIP-1 §6.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayLimits {
    /// Байт/с суммарно по всем цепочкам (по умолчанию 2 МБ/с).
    pub bandwidth: u64,
    pub max_circuits: u32,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self { bandwidth: 2 * 1024 * 1024, max_circuits: 64 }
    }
}

/// Таблица известных релеев (описатели из gossip + bootstrap без описателя).
#[derive(Clone, Default)]
pub struct Directory {
    inner: Arc<Mutex<DirInner>>,
}

#[derive(Default)]
struct DirInner {
    relays: HashMap<EndpointId, RelayDescriptor>,
    bootstrap: Vec<EndpointId>,
    /// Измеренная задержка рукопожатия, мс (для весов выбора).
    latency: HashMap<EndpointId, u32>,
    failures: HashMap<EndpointId, (u32, std::time::Instant)>,
    /// Подсказки адреса: из описателей, трекера и приложения.
    hints: HashMap<EndpointId, Vec<String>>,
    /// Описатели скрытого приёма по владельцу.
    services: HashMap<EndpointId, ServiceDescriptor>,
}

impl Directory {
    pub fn with_bootstrap(bootstrap: Vec<EndpointId>) -> Self {
        let d = Self::default();
        d.inner.lock().unwrap().bootstrap = bootstrap;
        d
    }
    /// Описатель из сети: принимаем только валидную подпись и свежий ts.
    pub fn insert(&self, d: RelayDescriptor) -> bool {
        if !d.verify() || !d.fresh() {
            return false;
        }
        let mut g = self.inner.lock().unwrap();
        if !d.addrs.is_empty() {
            g.hints.insert(d.node, d.addrs.clone());
        }
        match g.relays.get(&d.node) {
            Some(old) if old.ts >= d.ts => false,
            _ => {
                g.relays.insert(d.node, d);
                true
            }
        }
    }
    /// Подсказки адреса узла (заменяют прежние, пустой список игнорируется).
    pub fn add_hint(&self, node: EndpointId, addrs: Vec<String>) {
        if !addrs.is_empty() {
            self.inner.lock().unwrap().hints.insert(node, addrs);
        }
    }
    pub fn hints(&self, node: &EndpointId) -> Vec<String> {
        self.inner.lock().unwrap().hints.get(node).cloned().unwrap_or_default()
    }
    /// Адрес для `connect`: id + подсказки (прямое QUIC без discovery; реле — запасной путь).
    pub fn addr_of(&self, node: EndpointId) -> iroh::EndpointAddr {
        endpoint_addr(node, &self.hints(&node))
    }
    /// Узел ещё в сети: свежий описатель или bootstrap.
    pub fn is_known(&self, n: &EndpointId) -> bool {
        let g = self.inner.lock().unwrap();
        g.bootstrap.contains(n) || g.relays.get(n).is_some_and(|d| d.fresh())
    }
    /// Может быть последним хопом: описатель с Exit или bootstrap без описателя.
    pub fn can_exit(&self, n: &EndpointId) -> bool {
        let g = self.inner.lock().unwrap();
        match g.relays.get(n) {
            Some(d) => d.exit,
            None => g.bootstrap.contains(n),
        }
    }
    /// Число разных пригодных релеев (без `exclude`) — сколько хопов можно построить.
    pub fn usable(&self, exclude: &[EndpointId]) -> usize {
        let g = self.inner.lock().unwrap();
        let mut ids: Vec<EndpointId> = g.relays.values().filter(|d| d.fresh()).map(|d| d.node).chain(g.bootstrap.iter().copied()).filter(|n| !exclude.contains(n)).collect();
        ids.sort();
        ids.dedup();
        ids.len()
    }
    /// Описатель скрытого приёма из сети (подпись + свежесть + монотонный ts).
    pub fn insert_service(&self, d: ServiceDescriptor) -> bool {
        if !d.verify() || !d.fresh() {
            return false;
        }
        let mut g = self.inner.lock().unwrap();
        match g.services.get(&d.owner) {
            Some(old) if old.ts >= d.ts => false,
            _ => {
                g.services.insert(d.owner, d);
                true
            }
        }
    }
    pub fn service(&self, owner: &EndpointId) -> Option<ServiceDescriptor> {
        self.inner.lock().unwrap().services.get(owner).filter(|d| d.fresh()).cloned()
    }
    pub fn services_count(&self) -> usize {
        self.inner.lock().unwrap().services.values().filter(|d| d.fresh()).count()
    }
    pub fn note_latency(&self, n: EndpointId, ms: u32) {
        let mut g = self.inner.lock().unwrap();
        g.latency.insert(n, ms);
        g.failures.remove(&n);
    }
    pub fn note_failure(&self, n: EndpointId) {
        let mut g = self.inner.lock().unwrap();
        let e = g.failures.entry(n).or_insert((0, std::time::Instant::now()));
        if e.1.elapsed() > FAILURE_FORGET {
            e.0 = 0;
        }
        e.0 += 1;
        e.1 = std::time::Instant::now();
    }
    pub fn failures(&self, n: &EndpointId) -> u32 {
        self.inner.lock().unwrap().failures.get(n).filter(|f| f.1.elapsed() <= FAILURE_FORGET).map(|f| f.0).unwrap_or(0)
    }
    pub fn known(&self) -> Vec<RelayDescriptor> {
        let mut g = self.inner.lock().unwrap();
        g.relays.retain(|_, d| d.fresh());
        g.relays.values().cloned().collect()
    }
    pub fn count(&self) -> usize {
        self.known().len()
    }
    pub fn bootstrap(&self) -> Vec<EndpointId> {
        self.inner.lock().unwrap().bootstrap.clone()
    }
    pub fn add_bootstrap(&self, nodes: Vec<EndpointId>) {
        let mut g = self.inner.lock().unwrap();
        for n in nodes {
            if !g.bootstrap.contains(&n) {
                g.bootstrap.push(n);
            }
        }
    }

    /// Кандидаты на роль с весами: аптайм неизвестен — берём задержку и число сбоев;
    /// bootstrap-узлы без описателя годятся на любую роль (ранняя сеть).
    pub fn candidates(&self, role: Role, exclude: &[EndpointId], exclude_family: &[String]) -> Vec<(EndpointId, f64, String)> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<(EndpointId, f64, String)> = Vec::new();
        let weight = |n: &EndpointId, bw: u64| -> f64 {
            let lat = *g.latency.get(n).unwrap_or(&400) as f64;
            let fails = g.failures.get(n).filter(|f| f.1.elapsed() <= FAILURE_FORGET).map(|f| f.0).unwrap_or(0) as f64;
            let bwf = if bw == 0 { 1.0 } else { (bw as f64 / (2.0 * 1024.0 * 1024.0)).clamp(0.2, 4.0) };
            (1000.0 / (lat + 50.0)) * bwf / (1.0 + fails)
        };
        for d in g.relays.values() {
            let ok = match role {
                Role::Guard => d.guard,
                Role::Middle => d.middle,
                Role::Exit => d.exit,
            };
            if ok && d.fresh() && !exclude.contains(&d.node) && !(!d.family.is_empty() && exclude_family.contains(&d.family)) {
                out.push((d.node, weight(&d.node, d.bandwidth), d.family.clone()));
            }
        }
        for b in &g.bootstrap {
            if !exclude.contains(b) && !g.relays.contains_key(b) {
                out.push((*b, weight(b, 0) * 0.5, String::new()));
            }
        }
        out
    }

    /// Взвешенный случайный выбор.
    pub fn pick(&self, role: Role, exclude: &[EndpointId], exclude_family: &[String]) -> Option<(EndpointId, String)> {
        use rand::Rng;
        let c = self.candidates(role, exclude, exclude_family);
        let total: f64 = c.iter().map(|x| x.1).sum();
        if c.is_empty() || total <= 0.0 {
            return None;
        }
        let mut r = rand::thread_rng().gen_range(0.0..total);
        for (n, w, fam) in c {
            if r < w {
                return Some((n, fam));
            }
            r -= w;
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Guard,
    Middle,
    Exit,
}

/// Закреплённые Guard-узлы (NIP-1 §6.2): набор из 3 на 30 дней, хранится в JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuardSet {
    pub guards: Vec<EndpointId>,
    pub chosen_at: u64,
}

pub const GUARD_SET_SIZE: usize = 3;
pub const GUARD_SET_TTL_SECS: u64 = 30 * 24 * 3600;

impl GuardSet {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).filter(|g: &GuardSet| now_secs().saturating_sub(g.chosen_at) < GUARD_SET_TTL_SECS).unwrap_or_default()
    }
    /// Запись на диск — в блокирующем пуле tokio, чтобы не задерживать асинхронные задачи.
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            if let Some(p) = path.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::debug!("guards.json: {e}");
            }
        });
        Ok(())
    }
    /// Пополнить набор из директории (новые guard'ы выбираются взвешенно).
    pub fn refill(&mut self, dir: &Directory) {
        if self.guards.is_empty() {
            self.chosen_at = now_secs();
        }
        while self.guards.len() < GUARD_SET_SIZE {
            match dir.pick(Role::Guard, &self.guards, &[]) {
                Some((n, _)) => self.guards.push(n),
                None => break,
            }
        }
    }
}
