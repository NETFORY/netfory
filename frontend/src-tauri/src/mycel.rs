//! Mycelium в Netfory (Фаза 1): тумблер «Mycelium protocol (experimental)», mushroom-маршрутизация
//! прямых чат-соединений (chat-dm, chat-sync), режим релея на десктопе, автопауза dVPN.
use serde::{Deserialize, Serialize};

const KEY_CFG: &str = "cfg:mycelium";
const KEY_DVPN_WAS: &str = "cfg:mycelium:dvpn_was";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Settings {
    pub enabled: bool,
    /// Разрешить прямое соединение при недоступности Mycelium (иначе fail-closed).
    pub allow_direct: bool,
    /// Звонки через Mycelium (Фаза 2 — пока всегда напрямую).
    pub voice: bool,
    /// «Только клиент»: не принимать чужие цепочки.
    pub client_only: bool,
    /// Лимит канала релея, КБ/с.
    pub bandwidth_kb: u64,
    pub hops: u8,
    /// Bootstrap-релеи (EndpointId hex), редактируются в настройках.
    pub bootstrap: Vec<String>,
    /// Фаза 2: маскирующий трафик (PADDING-ячейки на простаивающих цепочках). Скрытый приём
    /// (rendezvous) включён всегда вместе с Mycelium — отдельного тумблера нет.
    pub cover_traffic: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, allow_direct: false, voice: false, client_only: false, bandwidth_kb: 2048, hops: 3, bootstrap: Vec::new(), cover_traffic: false }
    }
}

/// Зеркало настроек рядом с БД (`<app_data>/data/mycelium.json`): переживает сбой записи sled.
fn mirror_path() -> Option<std::path::PathBuf> {
    crate::os_app_data_dir().map(|b| b.join("data").join("mycelium.json"))
}

pub(crate) fn load() -> Settings {
    if let Some(s) = crate::DB.get(KEY_CFG.as_bytes()).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()) {
        return s;
    }
    // в БД пусто — берём зеркало и возвращаем его в БД
    let from_file: Option<Settings> = mirror_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    match from_file {
        Some(s) => {
            if let Ok(b) = serde_json::to_vec(&s) {
                let _ = crate::DB.insert(KEY_CFG.as_bytes(), b);
            }
            s
        }
        None => Settings::default(),
    }
}

fn save(s: &Settings) -> Result<(), String> {
    let b = serde_json::to_vec(s).map_err(|e| e.to_string())?;
    crate::DB.insert(KEY_CFG.as_bytes(), b.clone()).map_err(|e| e.to_string())?;
    // явный flush: настройки не должны зависеть от фонового сброса sled при выходе
    crate::DB.flush().map_err(|e| e.to_string())?;
    if let Some(p) = mirror_path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, &b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    crate::syslog::log("mycelium", format!("settings saved: {}", describe(s)));
    Ok(())
}

fn describe(s: &Settings) -> String {
    format!("enabled={} hops={} clientOnly={} allowDirect={} cover={} bw={}KB/s bootstrap={}", s.enabled, s.hops, s.client_only, s.allow_direct, s.cover_traffic, s.bandwidth_kb, s.bootstrap.len())
}

pub(crate) fn enabled() -> bool {
    load().enabled
}

/// Мобильные устройства релеем не становятся (NIP-1 §6.4).
const IS_MOBILE: bool = cfg!(any(target_os = "android", target_os = "ios"));

/// Встроенные bootstrap-релеи NETFORY (EndpointId узлов pq-web, запущенных с `--mycelium`).
/// Список дополняется автоматически узлами с флагом `mycelium` из `GET /pqweb` трекера
/// (`webproxy::mycelium_node_ids`), поэтому цепочки строятся сразу после включения.
pub(crate) const NETFORY_BOOTSTRAP: &[&str] = &[];

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MyceliumInfo {
    pub settings: Option<Settings>,
    pub status: Option<serde_json::Value>,
    pub started: bool,
    /// Отдельный endpoint Mycelium поднят (поднимается только при включении).
    pub running: bool,
    pub mobile: bool,
    pub node_id: String,
    /// EndpointId транспорта Mycelium (свой endpoint, не основной узел).
    pub transport_id: String,
    pub system_daemon: bool,
    pub socks_port: u16,
    pub dvpn_paused: bool,
    /// Живые цепочки для карты «Вы → Guard → Middle → Exit → собеседник».
    pub circuits: Vec<serde_json::Value>,
    /// Фаза 3: системный VPN (TUN на всём устройстве).
    pub vpn: crate::mycel_vpn::VpnInfo,
}

/// Автопауза dVPN: при включении Mycelium запоминаем состояние и выключаем, при выключении — возвращаем.
fn apply_dvpn(enabling: bool) {
    if enabling {
        if crate::webproxy::total() {
            let _ = crate::DB.insert(KEY_DVPN_WAS.as_bytes(), b"1".as_slice());
            let _ = crate::webproxy::webproxy_set_total(false);
            crate::syslog::log("mycelium", "dVPN paused: Mycelium enabled");
        }
    } else if matches!(crate::DB.get(KEY_DVPN_WAS.as_bytes()), Ok(Some(v)) if v.as_ref() == b"1") {
        let _ = crate::DB.remove(KEY_DVPN_WAS.as_bytes());
        let _ = crate::webproxy::webproxy_set_total(true);
        crate::syslog::log("mycelium", "dVPN resumed: Mycelium disabled");
    }
}

pub(crate) fn dvpn_paused() -> bool {
    matches!(crate::DB.get(KEY_DVPN_WAS.as_bytes()), Ok(Some(v)) if v.as_ref() == b"1")
}

#[cfg(feature = "p2p")]
mod imp {
    use super::*;
    use iroh::EndpointId;
    use mycelium::Mycelium;
    use std::sync::OnceLock;

    static MY: OnceLock<Mycelium> = OnceLock::new();
    static EP: OnceLock<iroh::Endpoint> = OnceLock::new();
    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    static INIT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn parse_ids(v: &[String]) -> Vec<EndpointId> {
        v.iter().filter_map(|s| s.trim().parse().ok()).collect()
    }

    /// Все bootstrap-релеи: из настроек + встроенные NETFORY + узлы pq-web с `mycelium` с трекера.
    fn all_bootstrap(s: &Settings) -> Vec<EndpointId> {
        let mut v = parse_ids(&s.bootstrap);
        v.extend(NETFORY_BOOTSTRAP.iter().filter_map(|s| s.parse::<EndpointId>().ok()));
        v.extend(parse_ids(&crate::webproxy::mycelium_node_ids()));
        v.sort();
        v.dedup();
        v
    }

    /// Адреса pq-web из heartbeat трекера: прямое QUIC к релеям без DNS/pkarr-discovery.
    fn tracker_hints() -> Vec<(EndpointId, Vec<String>)> {
        crate::webproxy::mycelium_node_hints().into_iter().filter_map(|(id, a)| Some((id.parse().ok()?, a))).collect()
    }

    /// Список pq-web с трекера обновился — подхватываем новые релеи NETFORY без перезапуска.
    pub(crate) fn on_tracker_nodes() {
        let Some(my) = MY.get() else { return };
        my.add_hints(tracker_hints());
        let ids = parse_ids(&crate::webproxy::mycelium_node_ids());
        if !ids.is_empty() {
            my.add_bootstrap(ids);
        }
    }

    /// Несколько знакомых узлов из книги пиров — запасной вход в gossip-топик директории.
    fn known_peers() -> Vec<EndpointId> {
        crate::peerbook::list(None).into_iter().filter_map(|p| p.node_id.parse().ok()).take(6).collect()
    }

    /// Пауза перед подъёмом Mycelium при старте: первые секунды сеть нужна кошельку и узлам sth-core.
    const START_DELAY: std::time::Duration = std::time::Duration::from_secs(8);

    /// Вызывается после подъёма основного узла (p2p::ensure), ничего не ждёт: Mycelium живёт на
    /// отдельном endpoint'е и поднимается в фоне только если включён.
    pub(crate) fn boot(app: tauri::AppHandle) {
        if APP.set(app).is_err() {
            return;
        }
        let s = load();
        crate::syslog::log("mycelium", format!("settings loaded: {}", describe(&s)));
        if s.enabled {
            tokio::spawn(async move {
                tokio::time::sleep(START_DELAY).await;
                // пользователь мог успеть поменять настройки — применяем актуальные
                let s = load();
                if s.enabled {
                    apply(&s).await;
                }
            });
        }
    }

    /// Поднять отдельный endpoint Mycelium (один раз; при ошибке — повтор при следующем вызове).
    async fn ensure_started() -> Option<&'static Mycelium> {
        if let Some(m) = MY.get() {
            return Some(m);
        }
        let _g = INIT.lock().await;
        if let Some(m) = MY.get() {
            return Some(m);
        }
        let app = APP.get()?.clone();
        let ep = match crate::p2p::mycelium_endpoint(&app).await {
            Ok(ep) => ep,
            Err(e) => {
                crate::syslog::log("mycelium", format!("endpoint failed: {e}"));
                return None;
            }
        };
        let data_dir = crate::ensure_data_dir(&app);
        let s = load();
        let cfg = mycelium::Config {
            hops: s.hops.clamp(1, 3),
            relay: false,
            pool_size: 2,
            bootstrap: all_bootstrap(&s),
            exit_policy: mycelium::ExitPolicy::default(),
            limits: mycelium::RelayLimits { bandwidth: s.bandwidth_kb * 1024, max_circuits: 64 },
            data_dir: data_dir.join("mycelium"),
            hidden: true,
            intro_points: 2,
            cover_traffic: s.cover_traffic,
        };
        let gossip = iroh_gossip::Gossip::builder().spawn(ep.clone());
        let my = match Mycelium::start(ep.clone(), Some(gossip.clone()), cfg, known_peers()).await {
            Ok(m) => m,
            Err(e) => {
                crate::syslog::log("mycelium", format!("failed to start: {e}"));
                return None;
            }
        };
        // скрытый приём адресуется EndpointId основного узла (его знают контакты) — ключ личности
        if let Some(main) = crate::p2p::endpoint_now() {
            my.set_identity(main.secret_key().clone());
        }
        my.add_hints(tracker_hints());
        // скрытый приём: запросы chat-dm/chat-sync, пришедшие через точку встречи, идут в те же обработчики
        my.set_service_handler(std::sync::Arc::new(|alpn: Vec<u8>, req: Vec<u8>| -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send>> {
            Box::pin(async move {
                match alpn.as_slice() {
                    b"netfory/chat-dm/2" => crate::p2p::mycelium_dm_accept(&req),
                    a if a == crate::chat::sync::CHAT_SYNC_ALPN => tauri::async_runtime::spawn_blocking(move || crate::chat::sync::respond(&req)).await.ok().flatten(),
                    _ => None,
                }
            })
        }));
        let router = iroh::protocol::Router::builder(ep.clone()).accept(mycelium::ALPN, my.handler()).accept(iroh_gossip::ALPN, gossip).spawn();
        std::mem::forget(router); // живёт весь процесс
        crate::syslog::log("mycelium", format!("own endpoint up: {} (isolated from wallet/chat node)", ep.id()));
        let _ = EP.set(ep);
        let _ = MY.set(my);
        crate::mycel_vpn::set_data_dir(data_dir);
        spawn_log_loop();
        crate::mycel_vpn::spawn_poll_loop();
        MY.get()
    }

    /// Endpoint Mycelium (для диагностики соединений).
    pub(crate) fn endpoint_now() -> Option<iroh::Endpoint> {
        EP.get().cloned()
    }

    /// Экземпляр Mycelium (после подъёма endpoint'а).
    pub(crate) fn instance() -> Option<&'static Mycelium> {
        MY.get()
    }

    async fn apply(s: &Settings) {
        let my = if s.enabled { ensure_started().await } else { MY.get() };
        let Some(my) = my else { return };
        my.set_hops(s.hops);
        my.set_bandwidth(s.bandwidth_kb * 1024);
        my.set_cover_traffic(s.cover_traffic);
        my.add_bootstrap(all_bootstrap(s));
        // системный myceliumd на этой машине уже релей — свой не поднимаем
        let system = system_daemon().await;
        my.set_relay_enabled(s.enabled && !s.client_only && !IS_MOBILE && !system);
        if s.enabled && !my.is_enabled() {
            my.join_peers(known_peers());
        }
        my.set_enabled(s.enabled);
    }

    /// Изменения числа цепочек/ошибки — в системный журнал (scope `mycelium`).
    fn spawn_log_loop() {
        tokio::spawn(async move {
            let mut last = (usize::MAX, usize::MAX, String::new());
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let Some(my) = MY.get() else { continue };
                if !my.is_enabled() {
                    continue;
                }
                let st = my.status();
                let cur = (st.circuits_ready, st.relays_known, st.last_error.clone());
                if cur != last {
                    let err = if st.last_error.is_empty() { String::new() } else { format!(" · error: {}", st.last_error) };
                    let short = if st.hops_reduced { format!(" · reduced to {} of {} hops (not enough relays)", st.hops_effective, st.hops) } else { String::new() };
                    crate::syslog::log("mycelium", format!("circuits {} · relays {} (usable {}) · served {} · cells {}{short}{err}", st.circuits_ready, st.relays_known, st.relays_usable, st.relay_circuits_total, st.relay_cells));
                    last = cur;
                }
            }
        });
    }

    /// Запрос-ответ к `target` через Mycelium.
    /// `None` — Mycelium выключен (или цепочка не удалась и разрешён прямой путь): идём напрямую.
    /// `Some(Err)` — fail-closed: напрямую нельзя.
    pub(crate) async fn try_request(target: EndpointId, alpn: &[u8], payload: &[u8], max: usize) -> Option<Result<Vec<u8>, String>> {
        let my = MY.get()?;
        if !my.is_enabled() {
            return None;
        }
        let r = my.request(target, alpn, payload, max);
        match tokio::time::timeout(std::time::Duration::from_secs(40), r).await {
            Ok(Ok(v)) => Some(Ok(v)),
            res => {
                let e = match res {
                    Ok(Err(e)) => e.to_string(),
                    _ => "circuit timeout".into(),
                };
                if load().allow_direct {
                    crate::syslog::log("mycelium", format!("Mycelium unavailable — falling back to direct ({e})"));
                    None
                } else {
                    crate::syslog::log("mycelium", format!("circuit not built, direct connection forbidden: {e}"));
                    Some(Err(e))
                }
            }
        }
    }

    /// Системный myceliumd на :9150 — проверяется не чаще раза в 30 с (статус опрашивается каждые 3 с,
    /// а TCP-зонд с таймаутом не должен задерживать ответ UI).
    static SYSTEM_DAEMON: std::sync::Mutex<(bool, Option<std::time::Instant>)> = std::sync::Mutex::new((false, None));

    async fn system_daemon() -> bool {
        let cached = {
            let g = SYSTEM_DAEMON.lock().unwrap();
            match g.1 {
                Some(t) if t.elapsed() < std::time::Duration::from_secs(30) => Some(g.0),
                _ => None,
            }
        };
        if let Some(v) = cached {
            return v;
        }
        let v = mycelium::socks::probe(mycelium::socks::DEFAULT_PORT).await;
        *SYSTEM_DAEMON.lock().unwrap() = (v, Some(std::time::Instant::now()));
        v
    }

    pub(crate) async fn info() -> MyceliumInfo {
        let my = MY.get();
        MyceliumInfo {
            settings: Some(load()),
            status: my.and_then(|m| serde_json::to_value(m.status()).ok()),
            started: APP.get().is_some(),
            running: my.is_some(),
            transport_id: EP.get().map(|e| e.id().to_string()).unwrap_or_default(),
            mobile: IS_MOBILE,
            node_id: crate::p2p::endpoint_now().map(|e| e.id().to_string()).unwrap_or_default(),
            system_daemon: system_daemon().await,
            socks_port: mycelium::socks::DEFAULT_PORT,
            dvpn_paused: dvpn_paused(),
            circuits: my.map(|m| m.circuits().into_iter().filter_map(|c| serde_json::to_value(c).ok()).collect()).unwrap_or_default(),
            vpn: crate::mycel_vpn::info(),
        }
    }

    pub(crate) fn circuit_for(node: &str) -> Option<serde_json::Value> {
        let id: EndpointId = node.parse().ok()?;
        serde_json::to_value(MY.get()?.circuit_for_target(&id)?).ok()
    }

    pub(crate) async fn rebuild(id: u32) -> Result<serde_json::Value, String> {
        let my = MY.get().ok_or("Mycelium is not running")?;
        let r = tokio::time::timeout(std::time::Duration::from_secs(45), my.rebuild(id, false)).await;
        match r {
            Ok(Ok(info)) => {
                crate::syslog::log("mycelium", format!("circuit {id:#x} rebuilt manually → {:#x}", info.id));
                serde_json::to_value(info).map_err(|e| e.to_string())
            }
            Ok(Err(e)) => {
                crate::syslog::log("mycelium", format!("rebuild of {id:#x} failed: {e}"));
                Err(e.to_string())
            }
            Err(_) => Err("circuit build timeout".into()),
        }
    }

    pub(crate) async fn set(s: Settings) {
        apply(&s).await
    }
}

#[cfg(feature = "p2p")]
pub(crate) use imp::{boot, endpoint_now, instance, on_tracker_nodes, try_request};

#[cfg(not(feature = "p2p"))]
pub(crate) fn on_tracker_nodes() {}

/// Пересобрать цепочку по id (кнопка «Построить новую цепочку» на карте): мёртвые хопы
/// исключаются, замена встаёт в тот же слот. Возвращает снимок новой цепочки.
/// IP хопов (из подсказок адреса директории/трекера) — для геопривязки на глобусе (GeoLite2 локально).
#[tauri::command]
pub(crate) async fn mycelium_hop_addrs(ids: Vec<String>) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    #[cfg(feature = "p2p")]
    if let Some(my) = imp::instance() {
        for id in ids {
            let Ok(eid) = id.parse::<iroh::EndpointId>() else { continue };
            let ip = my.directory().hints(&eid).into_iter().filter_map(|a| a.parse::<std::net::SocketAddr>().ok()).map(|a| a.ip()).find(|ip| !(ip.is_loopback() || ip.is_unspecified() || matches!(ip, std::net::IpAddr::V4(v) if v.is_private() || v.is_link_local())));
            if let Some(ip) = ip {
                out.insert(id, ip.to_string());
            }
        }
    }
    #[cfg(not(feature = "p2p"))]
    let _ = ids;
    out
}

#[tauri::command]
pub(crate) async fn mycelium_rebuild(circuit_id: u32) -> Result<serde_json::Value, String> {
    #[cfg(feature = "p2p")]
    {
        imp::rebuild(circuit_id).await
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = circuit_id;
        Err("Mycelium is not available in this build".into())
    }
}

#[tauri::command]
pub(crate) async fn mycelium_status() -> MyceliumInfo {
    #[cfg(feature = "p2p")]
    {
        imp::info().await
    }
    #[cfg(not(feature = "p2p"))]
    {
        MyceliumInfo { settings: Some(load()), mobile: IS_MOBILE, socks_port: 9150, dvpn_paused: dvpn_paused(), vpn: crate::mycel_vpn::info(), ..Default::default() }
    }
}

/// Цепочка собеседника личного чата (по STH-адресу): None — Mycelium выкл или цепочки ещё нет.
#[tauri::command]
pub(crate) async fn mycelium_circuit(peer_address: String) -> Option<serde_json::Value> {
    #[cfg(feature = "p2p")]
    {
        // async: синхронная команда выполнялась бы на главном потоке (поиск узла в БД + зондирование хопов)
        let node = tauri::async_runtime::spawn_blocking(move || crate::chat::peer_node_id(&peer_address)).await.ok().flatten()?;
        imp::circuit_for(&node)
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = peer_address;
        None
    }
}

#[tauri::command]
pub(crate) async fn mycelium_set(settings: Settings) -> Result<MyceliumInfo, String> {
    let mut s = settings;
    s.hops = s.hops.clamp(1, 3);
    s.bandwidth_kb = s.bandwidth_kb.clamp(64, 1024 * 1024);
    s.voice = false;
    s.bootstrap.retain(|b| !b.trim().is_empty());
    let was = load().enabled;
    save(&s)?;
    // Mycelium и dVPN pq-web не работают вместе: при включении Mycelium dVPN принудительно выключаем
    if s.enabled {
        crate::webproxy::force_total_off();
    }
    if s.enabled != was {
        apply_dvpn(s.enabled);
        crate::syslog::log("mycelium", if s.enabled { "enabled (experimental)" } else { "disabled" });
        // без Mycelium системный VPN не имеет транспорта — снимаем туннель
        if !s.enabled {
            crate::mycel_vpn::stop().await;
        }
    }
    #[cfg(feature = "p2p")]
    {
        // применение не должно подвесить UI: ждём не дольше 10 с, остальное доделается в фоне
        let task = tauri::async_runtime::spawn(imp::set(s));
        if tokio::time::timeout(std::time::Duration::from_secs(10), task).await.is_err() {
            crate::syslog::log("mycelium", "set: apply took > 10 s — continuing in background");
        }
    }
    Ok(mycelium_status().await)
}
