/*
 * SmartNet // Wallet push-events over iroh (SHIP-44, ALPN `sth/events/1`)
 *
 *   Одно QUIC-соединение к узлу sth-core вместо поллинга REST: кадры
 *   `balance` / `tx` / `mempoolTx` / `block` приходят push'ем и уходят во Vue
 *   событием `wallet-event`. Кадры — `[u32 LE len][JSON]` в обе стороны на одном
 *   bi-stream: клиент шлёт `subscribe` / `ping`, узел — `hello`, `subscribed`,
 *   события. Ping раз в 60 с (idle_timeout узла 300 с), реконнект с паузой
 *   1 → 30 с и переходом на следующий iroh-узел пула; `sinceHeight` хранится в
 *   sled — после реконнекта узел досылает пропущенное (replay ≤ 200 tx).
 *
 *   Author: TechnoL0g
 *   Created: 2026-06
 */

use serde::Serialize;
use std::sync::Mutex;
#[cfg(feature = "p2p")]
use tauri::Emitter;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamStatus {
    /// Стрим подключён и подписан.
    pub live: bool,
    /// Узел стрима (`api://…/sth-core`).
    pub node: String,
    /// Последняя высота сети по кадрам `block` / `tx` / `subscribed`.
    pub height: u64,
    /// Кадров получено за сессию.
    pub frames: u64,
    /// Epoch-мс последнего кадра `block`/`tx`/`subscribed` (v1.135.213).
    pub last_block_ms: u64,
    /// Стрим подключён, но блоки не идут > STALE_AFTER_MS — данные устарели.
    pub stale: bool,
}

/// Блок ~8 с: нет блоков дольше минуты — стрим «залип» (узел отвечает на ping, но
/// подписка/синхронизация встала). Поллинг снова включается, UI показывает STALE.
const STALE_AFTER_MS: u64 = 60_000;

/// Запрос принудительного реконнекта стрима (клик по бейджу STALE в UI).
static FORCE_RECONNECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

static STATE: Mutex<Option<StreamStatus>> = Mutex::new(None);

fn state() -> StreamStatus {
    let mut s = STATE.lock().unwrap().clone().unwrap_or_default();
    s.stale = s.live && s.last_block_ms > 0 && now_ms().saturating_sub(s.last_block_ms) > STALE_AFTER_MS;
    s
}

fn update(f: impl FnOnce(&mut StreamStatus)) {
    let mut g = STATE.lock().unwrap();
    let mut s = g.clone().unwrap_or_default();
    f(&mut s);
    *g = Some(s);
}

/// true — push-стрим живой И свежий (блоки идут): поллинг баланса можно не делать.
pub(crate) fn is_live() -> bool {
    let s = state();
    s.live && !s.stale
}

#[tauri::command]
pub(crate) fn wallet_stream_status() -> StreamStatus {
    state()
}

/// Принудительный реконнект стрима (с ротацией узла). Срабатывает на ближайшем тике (≤10 с).
#[tauri::command]
pub(crate) fn wallet_stream_reconnect() {
    FORCE_RECONNECT.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::syslog::log("net", "wallet-stream: реконнект по запросу пользователя".to_string());
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn spawn(_app: tauri::AppHandle) {}

/// Супервизор стрима (v1.135.221). Раньше `run` запускался один раз: паника в любом
/// обработчике кадра (sled/parse/unwrap) или зависший без таймаута await убивали стрим
/// до перезапуска приложения («REST #N» на часы, помогал только рестарт). Теперь:
/// * паника/завершение `run` → лог и перезапуск через 5 с;
/// * сторожевой таймер: стрим не live дольше `WATCHDOG_DEAD`, при этом сеть есть —
///   текущий `run` прерывается (abort) и запускается заново с чистым состоянием.
#[cfg(feature = "p2p")]
pub(crate) fn spawn(app: tauri::AppHandle) {
    const WATCHDOG_DEAD: std::time::Duration = std::time::Duration::from_secs(300);
    tauri::async_runtime::spawn(async move {
        let mut generation = 0u32;
        loop {
            generation += 1;
            let mut handle = tokio::spawn(p2p::run(app.clone()));
            let started = std::time::Instant::now();
            let reason = loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if handle.is_finished() {
                    break match (&mut handle).await {
                        Ok(()) => "цикл стрима завершился".to_string(),
                        Err(e) => format!("цикл стрима упал: {e}"),
                    };
                }
                let st = state();
                let last = if st.last_block_ms > 0 { now_ms().saturating_sub(st.last_block_ms) } else { started.elapsed().as_millis() as u64 };
                if !st.live && crate::netwatch::is_online() && started.elapsed() > WATCHDOG_DEAD && last > WATCHDOG_DEAD.as_millis() as u64 {
                    handle.abort();
                    break format!("стрим не live {} с — сторож перезапускает цикл", last / 1000);
                }
            };
            crate::syslog::log("net", format!("wallet-stream: {reason} (поколение {generation}) — перезапуск через 5 с"));
            update(|s| {
                s.live = false;
                s.frames = 0;
            });
            let _ = app.emit("wallet-event", serde_json::json!({ "type": "stream", "live": false, "node": "", "height": state().height }));
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

#[cfg(feature = "p2p")]
mod p2p {
    use super::update;
    use serde_json::{json, Value};
    use std::time::Duration;
    use tauri::{AppHandle, Emitter};

    const EVENTS_ALPN: &[u8] = b"sth/events/1";
    const MAX_FRAME: usize = 8 * 1024 * 1024;
    const PING_EVERY: Duration = Duration::from_secs(60);
    /// Тик служебных проверок (stale / force-reconnect / смена кошелька).
    const TICK: Duration = Duration::from_secs(10);
    /// Без единого кадра дольше этого — соединение считаем мёртвым (узел шлёт
    /// `block` каждые ~8 с и `pong` на наш ping).
    const SILENCE_LIMIT: Duration = Duration::from_secs(150);
    /// v1.135.212: нет кадров `block` дольше этого (блок ~8 с) — узел отвечает на ping,
    /// но его подписка/синхронизация встала («LIVE #N» не растёт, входящие не приходят).
    const BLOCK_STALE: Duration = Duration::from_secs(45);

    fn since_key(addr: &str) -> String {
        format!("wstream:height:{}", addr)
    }

    fn load_since(addr: &str) -> Option<u64> {
        crate::DB
            .get(since_key(addr).as_bytes())
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8_lossy(&v).parse().ok())
    }

    fn save_since(addr: &str, h: u64) {
        if h > 0 {
            let _ = crate::DB.insert(since_key(addr).as_bytes(), h.to_string().into_bytes());
        }
    }

    async fn write_frame(send: &mut iroh::endpoint::SendStream, v: &Value) -> Result<(), String> {
        let data = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&data);
        send.write_all(&buf).await.map_err(|e| e.to_string())
    }

    async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<Value, String> {
        let mut h = [0u8; 4];
        // EOF на границе кадра = узел штатно закрыл стрим (idle/рестарт) → реконнект
        recv.read_exact(&mut h).await.map_err(|e| match e {
            iroh::endpoint::ReadExactError::FinishedEarly(0) => "узел закрыл стрим".to_string(),
            other => other.to_string(),
        })?;
        let len = u32::from_le_bytes(h) as usize;
        if len > MAX_FRAME {
            return Err("кадр слишком большой".into());
        }
        let mut p = vec![0u8; len];
        recv.read_exact(&mut p).await.map_err(|e| e.to_string())?;
        serde_json::from_slice(&p).map_err(|e| format!("кадр не JSON: {e}"))
    }

    /// EndpointId из `api://<id>/<name>`.
    fn endpoint_id(node: &str) -> Result<iroh::EndpointId, String> {
        let rest = node.strip_prefix("api://").ok_or("узел стрима должен быть api://")?;
        let id = rest.split('/').next().unwrap_or("");
        id.parse().map_err(|_| format!("невалидный EndpointId узла: {}", crate::sth_node::short_host(node)))
    }

    /// Узел для стрима: активный, если он iroh; иначе — по кругу из iroh-пула.
    /// Узлы, у которых стрим «залипал» (pong есть, блоков нет): host → когда.
    /// Исключаются из ротации стрима на STUCK_BAN; счётчик — для syslog.
    static STUCK: std::sync::Mutex<Option<std::collections::HashMap<String, (std::time::Instant, u32)>>> = std::sync::Mutex::new(None);
    const STUCK_BAN: Duration = Duration::from_secs(3600);

    fn mark_stuck(node: &str, stream_h: u64, net_h: u64) {
        let mut g = STUCK.lock().unwrap();
        let m = g.get_or_insert_with(Default::default);
        let e = m.entry(node.to_string()).or_insert((std::time::Instant::now(), 0));
        e.0 = std::time::Instant::now();
        e.1 += 1;
        crate::syslog::log(
            "net",
            format!(
                "wallet-stream: узел {} залип (#{} при сети #{}), {}-й раз — исключён из ротации стрима на 60 мин",
                crate::sth_node::short_host(node),
                stream_h,
                net_h,
                e.1
            ),
        );
    }

    fn is_stuck(node: &str) -> bool {
        let g = STUCK.lock().unwrap();
        g.as_ref().and_then(|m| m.get(node)).map_or(false, |(t, _)| t.elapsed() < STUCK_BAN)
    }

    fn pick_node(attempt: usize) -> Option<String> {
        let active = crate::sth_node::active_host();
        if active.starts_with("api://") && attempt == 0 && !is_stuck(&active) {
            return Some(active);
        }
        let pool = crate::sth_node::iroh_nodes();
        if pool.is_empty() {
            return None;
        }
        // сначала не «залипавшие»; если забанены все — берём по кругу, как раньше
        let fresh: Vec<&String> = pool.iter().filter(|n| !is_stuck(n)).collect();
        if fresh.is_empty() {
            return Some(pool[attempt % pool.len()].clone());
        }
        Some(fresh[attempt % fresh.len()].clone())
    }

    /// Адрес активного аккаунта кошелька (ключ sinceHeight / переподписка при смене).
    fn vault_address() -> String {
        crate::wallet::active_address()
    }

    /// Все адреса аккаунтов сида (main + BIP-44) — стрим подписывается на все.
    fn all_addresses(active: &str) -> Vec<String> {
        let mut v: Vec<String> = crate::wallet::all_account_addresses();
        if !v.iter().any(|a| a == active) {
            v.push(active.to_string());
        }
        v
    }

    fn emit_stream(app: &AppHandle, live: bool, node: &str) {
        update(|s| {
            s.live = live;
            s.node = node.to_string();
            if !live {
                s.frames = 0;
            }
        });
        let h = super::state().height;
        let _ = app.emit("wallet-event", json!({ "type": "stream", "live": live, "node": node, "height": h }));
    }

    pub(super) async fn run(app: AppHandle) {
        let mut backoff = 1u64;
        let mut attempt = 0usize;
        // даём iroh-эндпоинту и UI подняться
        tokio::time::sleep(Duration::from_secs(4)).await;
        loop {
            if !crate::netwatch::is_online() {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            let address = vault_address();
            if address.is_empty() {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            // локальный полный индекс истории владельца (идемпотентно)
            crate::txindex::ensure(app.clone(), all_addresses(&address));
            let Some(node) = pick_node(attempt) else {
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            };
            match session(&app, &node, &address).await {
                Ok(()) => {
                    backoff = 1;
                    attempt = 0;
                    emit_stream(&app, false, &node);
                    continue; // штатное завершение (смена адреса / resync) — сразу заново
                }
                Err(e) => {
                    crate::syslog::log(
                        "net",
                        format!("wallet-stream {}: {} — реконнект через {}с", crate::sth_node::short_host(&node), e, backoff),
                    );
                    emit_stream(&app, false, &node);
                    attempt += 1;
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }

    async fn session(app: &AppHandle, node: &str, address: &str) -> Result<(), String> {
        let eid = endpoint_id(node)?;
        // R1: каждая фаза установления — с таймаутом; раньше open_bi/первая запись могли
        // висеть бесконечно на полумёртвом QUIC-соединении и стрим «умирал» до рестарта.
        let endpoint = tokio::time::timeout(Duration::from_secs(20), crate::p2p::ai_iroh_endpoint(app))
            .await
            .map_err(|_| "iroh endpoint не поднялся за 20с".to_string())??;
        // адреса узла из sled-книги пиров (без ожидания discovery), fallback — discovery
        let conn = crate::p2p::connect_known(&endpoint, eid, EVENTS_ALPN, Duration::from_secs(20))
            .await
            .map_err(|e| format!("connect: {e}"))?;
        crate::peerbook::remember(&endpoint, eid, crate::peerbook::Role::Wallet).await;
        let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(15), conn.open_bi())
            .await
            .map_err(|_| "open_bi: таймаут 15с".to_string())?
            .map_err(|e| e.to_string())?;

        // QUIC: узел увидит наш bi-stream только после первой записи — поэтому
        // subscribe уходит сразу, hello/subscribed читаем следом в любом порядке.
        // Референс-клиент sth-core: ключ sinceHeight присутствует только при replay.
        let mut sub = json!({
            "type": "subscribe",
            "addresses": all_addresses(address),
            "topics": ["wallet", "mempool", "blocks"],
            "tokens": [],
        });
        if let Some(h) = load_since(address) {
            sub["sinceHeight"] = json!(h);
        }
        tokio::time::timeout(Duration::from_secs(15), write_frame(&mut send, &sub))
            .await
            .map_err(|_| "subscribe: таймаут записи 15с".to_string())??;

        // Читатель в отдельной задаче: read_exact не cancel-safe, а select!
        // с ping-таймером отменял бы его на полпути.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Value, String>>(64);
        let reader = tokio::spawn(async move {
            loop {
                let r = read_frame(&mut recv).await;
                let stop = r.is_err();
                if tx.send(r).await.is_err() || stop {
                    break;
                }
            }
        });

        let mut tick = tokio::time::interval(TICK);
        tick.tick().await; // первый тик мгновенный — пропускаем
        let mut subscribed = false;
        let mut last_block = std::time::Instant::now();
        let mut last_ping = std::time::Instant::now();
        super::FORCE_RECONNECT.store(false, std::sync::atomic::Ordering::SeqCst);
        let result = loop {
            tokio::select! {
                f = tokio::time::timeout(SILENCE_LIMIT, rx.recv()) => {
                    let frame = match f {
                        Ok(Some(Ok(v))) => v,
                        Ok(Some(Err(e))) => break Err(format!("стрим закрыт: {e}")),
                        Ok(None) => break Err("стрим закрыт".to_string()),
                        Err(_) => break Err("тишина в стриме > 150с".to_string()),
                    };
                    if matches!(frame["type"].as_str(), Some("block") | Some("tx") | Some("subscribed")) {
                        last_block = std::time::Instant::now();
                        update(|s| s.last_block_ms = super::now_ms());
                    }
                    match handle_frame(app, node, address, frame, &mut subscribed) {
                        Flow::Continue => {}
                        Flow::Resync => {
                            let _ = crate::DB.remove(since_key(address).as_bytes());
                            crate::sth_node::bust_history_throttle(address);
                            break Ok(());
                        }
                    }
                }
                _ = tick.tick() => {
                    if vault_address() != address {
                        break Ok(()); // кошелёк сменился/удалён — переподписка
                    }
                    if super::FORCE_RECONNECT.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        let _ = crate::DB.remove(since_key(address).as_bytes());
                        crate::sth_node::bust_history_throttle(address);
                        break Err("реконнект по запросу (STALE)".to_string());
                    }
                    if last_ping.elapsed() >= PING_EVERY {
                        last_ping = std::time::Instant::now();
                        // запись внутри ветки select блокирует весь цикл — только с таймаутом
                        match tokio::time::timeout(Duration::from_secs(15), write_frame(&mut send, &json!({ "type": "ping" }))).await {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => break Err(format!("ping: {e}")),
                            Err(_) => break Err("ping: таймаут записи 15с — соединение зависло".to_string()),
                        }
                    }
                    // блоки не идут, хотя pong есть → сверяем высоту с остальным пулом
                    if subscribed && last_block.elapsed() >= BLOCK_STALE {
                        let stream_h = super::state().height;
                        let net_h = tokio::time::timeout(Duration::from_secs(25), tokio::task::spawn_blocking(crate::sth_node::ping_all))
                            .await
                            .ok()
                            .and_then(|r| r.ok())
                            .map(|v| v.iter().filter_map(|p| p.height()).max().unwrap_or(0))
                            .unwrap_or(0);
                        if net_h > stream_h + 2 {
                            // стрим отстал от сети: узел не синхронизирован или подписка умерла
                            mark_stuck(node, stream_h, net_h);
                            let _ = crate::DB.remove(since_key(address).as_bytes());
                            crate::sth_node::bust_history_throttle(address);
                            let _ = app.emit("wallet-event", json!({ "type": "resync", "reason": "stale-stream" }));
                            break Err(format!("стрим без блоков {}с: #{} при сети #{} — смена узла", last_block.elapsed().as_secs(), stream_h, net_h));
                        }
                        if net_h == 0 {
                            break Err(format!("стрим без блоков {}с, пул не отвечает — реконнект", last_block.elapsed().as_secs()));
                        }
                        // сеть тоже стоит — узел не виноват, ждём дальше
                        last_block = std::time::Instant::now();
                    }
                }
            }
        };
        reader.abort();
        conn.close(0u32.into(), b"bye");
        result
    }

    enum Flow {
        Continue,
        Resync,
    }

    fn handle_frame(app: &AppHandle, node: &str, address: &str, mut frame: Value, subscribed: &mut bool) -> Flow {
        let kind = frame["type"].as_str().unwrap_or("").to_string();
        update(|s| s.frames += 1);
        match kind.as_str() {
            "hello" => {
                if let Some(h) = frame["height"].as_u64() {
                    update(|s| s.height = h);
                }
                crate::syslog::log(
                    "net",
                    format!(
                        "wallet-stream {}: hello core {} height {}",
                        crate::sth_node::short_host(node),
                        frame["coreVersion"].as_str().unwrap_or("?"),
                        frame["height"].as_u64().unwrap_or(0)
                    ),
                );
                return Flow::Continue;
            }
            "subscribed" => {
                let h = frame["height"].as_u64().unwrap_or(0);
                update(|s| s.height = h);
                save_since(address, h);
                if !*subscribed {
                    *subscribed = true;
                    crate::syslog::log(
                        "net",
                        format!(
                            "wallet-stream {}: подписка ок, height {}, replayed {}",
                            crate::sth_node::short_host(node),
                            h,
                            frame["replayed"].as_u64().unwrap_or(0)
                        ),
                    );
                    emit_stream(app, true, node);
                }
            }
            "block" => {
                let h = frame["block"]["height"].as_u64().unwrap_or(0);
                update(|s| s.height = h);
                save_since(address, h);
            }
            "tx" | "mempoolTx" => {
                let confirmed = frame["type"] == "tx";
                if confirmed {
                    if let Some(h) = frame["height"].as_u64() {
                        update(|s| s.height = h);
                        save_since(address, h);
                    }
                }
                // Перевод между СВОИМИ адресами (main → суб-аккаунт) — одна tx, но
                // две перспективы: у отправителя «-12», у получателя «+12». Кадр
                // раскладываем по каждому нашему адресу-участнику; иначе узел
                // подставлял адрес подписки и получатель видел чужой «out».
                let mut targets: Vec<String> = all_addresses(address)
                    .into_iter()
                    .filter(|a| tx_involves(&frame["tx"], a))
                    .collect();
                if targets.is_empty() {
                    targets.push(frame["address"].as_str().unwrap_or(address).to_string());
                }
                for addr in targets {
                    let mut item = crate::sth_node::parse_hist_item(&frame["tx"], &addr);
                    if confirmed {
                        crate::sth_node::bust_history_throttle(&addr);
                        if item.kind == "in" && !item.id.is_empty() {
                            notify_incoming(app, &addr, &item);
                        }
                        // подтверждённая tx → локальный индекс истории (без опроса цепи);
                        // blockId может лежать на уровне кадра — upsert требует его в tx
                        let mut tx = frame["tx"].clone();
                        if tx["blockId"].as_str().map_or(true, |b| b.is_empty()) {
                            if let Some(b) = frame["blockId"].as_str().or_else(|| frame["block"]["id"].as_str()) {
                                tx["blockId"] = Value::String(b.to_string());
                            }
                        }
                        crate::txindex::upsert(&addr, &tx);
                        // `sub:*` — ребро графа подписок в sled без опроса цепи
                        crate::subs::on_stream_tx(app, &addr, &frame["tx"]);
                    } else {
                        item.confirmed = false;
                        if item.timestamp == 0 {
                            item.timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                        }
                        // в мемпуле — в локальный pending-набор адреса (переживёт refresh)
                        crate::txindex::add_pending(&addr, item.clone());
                    }
                    let mut f = frame.clone();
                    f["address"] = Value::String(addr);
                    f["item"] = serde_json::to_value(&item).unwrap_or(Value::Null);
                    let _ = app.emit("wallet-event", f);
                }
                return if confirmed { Flow::Resync } else { Flow::Continue };
            }
            "balance" => {
                let raw: u64 = frame["wallet"]["balance"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                frame["balanceSth"] = Value::String(crate::sth_node::fmt_balance_sth(raw));
                // SHIP-14: attributes.tokens {tokenId: balance} → список для мгновенного
                // обновления сумм на дашборде (манифесты фронт держит из get_wallet_tokens).
                if let Some(map) = frame["wallet"]["attributes"]["tokens"].as_object() {
                    let list: Vec<Value> = map
                        .iter()
                        .map(|(id, b)| {
                            let bal = b.as_str().map(|s| s.to_string()).or_else(|| b.as_u64().map(|n| n.to_string())).unwrap_or_default();
                            json!({ "id": id, "balance": bal })
                        })
                        .collect();
                    frame["tokens"] = Value::Array(list);
                }
            }
            "rollback" => {
                crate::sth_node::bust_history_throttle(address);
                crate::syslog::log("net", format!("wallet-stream: rollback до {}", frame["height"].as_u64().unwrap_or(0)));
            }
            "resync" => {
                crate::syslog::log("net", format!("wallet-stream: resync ({})", frame["reason"].as_str().unwrap_or("")));
                let _ = app.emit("wallet-event", frame);
                return Flow::Resync;
            }
            "error" => {
                crate::syslog::log("net", format!("wallet-stream: ошибка узла: {}", frame["message"].as_str().unwrap_or("")));
            }
            "pong" => return Flow::Continue,
            _ => {}
        }
        let _ = app.emit("wallet-event", frame);
        Flow::Continue
    }

    /// Участвует ли адрес в tx: отправитель, получатель или получатель multipayment.
    fn tx_involves(tx: &Value, addr: &str) -> bool {
        tx["sender"].as_str() == Some(addr)
            || tx["recipient"].as_str() == Some(addr)
            || tx["asset"]["payments"]
                .as_array()
                .map_or(false, |ps| ps.iter().any(|p| p["recipientId"].as_str() == Some(addr)))
    }

    /// Входящая подтверждённая tx: тост/уведомление через существующее событие
    /// `sth-balance-changed` (App.vue) с дедупом по id в sled — тот же набор
    /// `wallet:seen_in:<addr>`, что и у поллинг-watcher'а, чтобы после
    /// отключения стрима он не уведомил повторно.
    fn notify_incoming(app: &AppHandle, address: &str, item: &crate::sth_node::HistItem) {
        let key = format!("wallet:seen_in:{}", address);
        let mut seen: std::collections::HashSet<String> = crate::DB
            .get(key.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        if !seen.insert(item.id.clone()) {
            return;
        }
        if let Ok(bytes) = serde_json::to_vec(&seen) {
            let _ = crate::DB.insert(key.as_bytes(), bytes);
        }
        let amount_sth: f64 = item.amount.parse().unwrap_or(0.0);
        let raw = (amount_sth * 100_000_000.0).round() as u64;
        let _ = app.emit(
            "sth-balance-changed",
            json!({
                "amount": raw,
                "amountSth": crate::sth_node::fmt_balance_sth(raw),
                "sender": item.counterparty.clone(),
                "txid": item.id.clone(),
            }),
        );
    }
}
