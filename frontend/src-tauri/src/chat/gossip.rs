//! Phase 2: Iroh Gossipsub-транспорт бесплатных сообщений чата.
//! Вынесено из chat/mod.rs в отдельный файл (декларация — `pub(crate) mod gossip;`).

    use super::{decrypt, load_room, room_key, store_inbound, inbound_allowed, my_address, ChatMsg, DB};
    use iroh_gossip::api::{Event, GossipReceiver};
    use iroh_gossip::{Gossip, TopicId};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use once_cell::sync::Lazy;
    use tauri::AppHandle;

    /// Max cipher size pushed onto a topic. Gossip frame limit is the default
    /// 4096 (как в v1.135.50): 4096 − стелс-паддинг (≤521) − служебный оверхед
    /// протокола ≈ 3300. Всё крупнее уходит ТОЛЬКО прямым каналом (chat-dm).
    pub(crate) const MAX_PACKET: usize = 3300;

    static GOSSIP: Lazy<Mutex<Option<Gossip>>> = Lazy::new(|| Mutex::new(None));
    static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));
    // room_id -> outbound sender handle
    static SENDERS: Lazy<Mutex<HashMap<String, iroh_gossip::api::GossipSender>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    // room_id -> set of connected neighbor EndpointIds (live presence on the topic)
    static PEERS: Lazy<Mutex<HashMap<String, HashSet<String>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    // Комнаты, покинутые явно (leave) — их авто-ресабскрайб цикл должен завершиться.
    static LEFT: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
    // Время последнего жёсткого ресета подписки по комнате (анти-шторм, ≥10 мин).
    static LAST_RESET: Lazy<Mutex<HashMap<String, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
    // Комнаты с живым циклом подписки (защита от дублей при гонке join/resubscribe).
    static PUMPS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
    // Время последнего ЛЮБОГО события с топика (room_id → unix-ms): пища для
    // зомби-детектора. В живой комнате пиры шлют sync-анонсы каждые ~70с, так
    // что тишина >5 мин при «живых» соседях = подписка умерла молча.
    static LAST_RX: Lazy<Mutex<HashMap<String, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
    // Watchdog-эскалация: комнаты, которым велено пересоздать подписку целиком.
    static RESET: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
    /// Комнаты, ждущие пересаба после возврата сети (для плашки «Reconnecting mesh…»).
    static RECONNECTING: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

    /// Событие фронту о состоянии меша: reconnecting (n комнат) | ready.
    fn emit_mesh(state: &str, rooms: usize) {
        if let Some(app) = APP.lock().unwrap().clone() {
            use tauri::Emitter;
            let _ = app.emit("chat-mesh", serde_json::json!({ "state": state, "rooms": rooms }));
        }
    }
    static RESET_NOTIFY: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
    // Outbox: пакеты, не ушедшие в mesh (подписка мертва/broadcast упал).
    // Досылаются при NeighborUp / после переподписки / фоновым флашером.
    // Без него сообщения, написанные в изолированный топик, ТЕРЯЛИСЬ навсегда
    // («пишет, но никто не видит») — анти-энтропия лечит только при живом tx.
    static OUTBOX: Lazy<Mutex<HashMap<String, Vec<(u64, Vec<u8>)>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn outbox_push(room_id: &str, payload: Vec<u8>) {
        let mut ob = OUTBOX.lock().unwrap();
        let q = ob.entry(room_id.to_string()).or_default();
        q.push((super::now_ms(), payload));
        let len = q.len();
        if len > 50 {
            q.drain(..len - 50);
        }
    }

    /// Дослать очередь комнаты (вызов дешёвый: пустая очередь — мгновенный выход).
    fn flush_outbox(room_id: &str) {
        let items: Vec<(u64, Vec<u8>)> = match OUTBOX.lock().unwrap().remove(room_id) {
            Some(v) if !v.is_empty() => v,
            _ => return,
        };
        let tx = SENDERS.lock().unwrap().get(room_id).cloned();
        let Some(tx) = tx else {
            OUTBOX.lock().unwrap().entry(room_id.to_string()).or_default().extend(items);
            return;
        };
        let rid = room_id.to_string();
        tauri::async_runtime::spawn(async move {
            let now = super::now_ms();
            let mut failed: Vec<(u64, Vec<u8>)> = Vec::new();
            let mut sent = 0u32;
            for (ts, p) in items {
                // старше суток — уже дошло анти-энтропией либо неактуально
                if now.saturating_sub(ts) > 24 * 3600 * 1000 {
                    continue;
                }
                if tx.broadcast(p.clone().into()).await.is_err() {
                    failed.push((ts, p));
                } else {
                    sent += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
            }
            if sent > 0 {
                crate::syslog::log("chat", format!("outbox: re-sent {sent} queued packet(s)"));
            }
            if !failed.is_empty() {
                OUTBOX.lock().unwrap().entry(rid).or_default().extend(failed);
            }
        });
    }

    /// Фоновый флашер: раз в 30с пытается дослать все непустые очереди.
    fn spawn_outbox_flusher() {
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let rooms: Vec<String> = OUTBOX.lock().unwrap().keys().cloned().collect();
                for r in rooms {
                    flush_outbox(&r);
                }
            }
        });
    }

    /// Number of live gossip neighbors on a room's topic (peer presence).
    pub(crate) fn room_peer_count(room_id: &str) -> usize {
        PEERS.lock().unwrap().get(room_id).map(|s| s.len()).unwrap_or(0)
    }

    /// Перепись памяти crashdiag: (сообщений в outbox, байт в outbox).
    pub(crate) fn outbox_size() -> (usize, usize) {
        let ob = OUTBOX.lock().unwrap();
        let items = ob.values().map(|q| q.len()).sum();
        let bytes = ob.values().flat_map(|q| q.iter().map(|(_, b)| b.len())).sum();
        (items, bytes)
    }

    /// Активных topic-подписок (перепись памяти crashdiag).
    pub(crate) fn topic_count() -> usize {
        SENDERS.lock().unwrap().len()
    }

    /// Комнаты с активной gossip-подпиской (для анти-энтропийного sync-цикла).
    pub(crate) fn joined_rooms() -> Vec<String> {
        SENDERS.lock().unwrap().keys().cloned().collect()
    }

    /// Вход прямой доставки (ALPN chat-dm / chat-sync): тот же конвейер, что и у
    /// gossip — расшифровка, дедуп по id конверта, квитанция. v1.135.229: если
    /// конверт группы оказался НОВЫМ для нас — ре-бродкастим его в свой gossip-
    /// остров (мост между островами: соседи получат то, что до них не дошло;
    /// дубликаты у них отсеет дедуп по id).
    pub(crate) fn handle_direct(room_id: String, cipher: String) {
        tauri::async_runtime::spawn(async move {
            let (rid, c) = (room_id.clone(), cipher.clone());
            let fresh = tauri::async_runtime::spawn_blocking(move || handle_inbound_blocking(&rid, &c))
                .await
                .unwrap_or(false);
            let is_group = load_room(&room_id).map(|r| r.kind == "group").unwrap_or(false);
            if fresh && is_group && cipher.len() <= MAX_PACKET {
                broadcast(room_id, cipher);
            }
        });
    }

    /// Текущие gossip-соседи комнаты (EndpointId) — для реестра участников chat-sync.
    pub(crate) fn room_peers(room_id: &str) -> Vec<String> {
        PEERS.lock().unwrap().get(room_id).map(|s| s.iter().cloned().collect()).unwrap_or_default()
    }

    fn emit_peers(room_id: &str) {
        let n = room_peer_count(room_id);
        if let Some(app) = APP.lock().unwrap().clone() {
            use tauri::Emitter;
            let _ = app.emit(
                "chat-peers",
                serde_json::json!({ "roomId": room_id, "neighbors": n }),
            );
        }
    }

    /// Живое событие «печатает…» для открытого чата (эфемерно, без записи в БД).
    fn emit_typing(room_id: &str, sender: &str) {
        if let Some(app) = APP.lock().unwrap().clone() {
            use tauri::Emitter;
            let _ = app.emit(
                "chat-typing",
                serde_json::json!({ "roomId": room_id, "sender": sender }),
            );
        }
    }

    /// Live transit status for the messenger HUD.
    pub(crate) fn status() -> super::GossipStatus {
        let topics = SENDERS.lock().unwrap().len();
        // комнаты, у которых цикл подписки жив, но live-sender отсутствует =
        // идёт переподписка (Lagged-бэкофф / watchdog-ресет)
        let resubscribing = {
            let senders = SENDERS.lock().unwrap();
            PUMPS.lock().unwrap().iter().filter(|r| !senders.contains_key(*r)).count()
        };
        let outbox = OUTBOX.lock().unwrap().values().map(|q| q.len()).sum();
        super::GossipStatus {
            online: GOSSIP.lock().unwrap().is_some(),
            topics,
            p2p: true,
            resubscribing,
            outbox,
        }
    }

    /// Drop all topic subscriptions + presence state (used on identity reset).
    /// Сеть вернулась: полный ресаб ВСЕХ живых топиков + сброс троттла watchdog'а
    /// (обычный путь — только раз в 10 мин на комнату — тут неуместен).
    pub(crate) fn resubscribe_all() {
        let rooms: Vec<String> = SENDERS.lock().unwrap().keys().cloned().collect();
        if rooms.is_empty() {
            return;
        }
        crate::syslog::log(
            "chat",
            format!("network recovered — resubscribing {} gossip topic(s)", rooms.len()),
        );
        let now = super::now_ms();
        {
            let mut lr = LAST_RESET.lock().unwrap();
            let mut rx = LAST_RX.lock().unwrap();
            let mut rs = RESET.lock().unwrap();
            let mut rc = RECONNECTING.lock().unwrap();
            for r in &rooms {
                lr.insert(r.clone(), now);
                rx.insert(r.clone(), now);
                rs.insert(r.clone());
                rc.insert(r.clone());
            }
        }
        emit_mesh("reconnecting", rooms.len());
        RESET_NOTIFY.notify_waiters();
    }

    pub(crate) fn reset() {
        SENDERS.lock().unwrap().clear();
        PEERS.lock().unwrap().clear();
    }

    /// Drop a single room's subscription + presence (leave/delete a chat/group).
    pub(crate) fn leave(room_id: String) {
        LEFT.lock().unwrap().insert(room_id.clone());
        SENDERS.lock().unwrap().remove(&room_id);
        PEERS.lock().unwrap().remove(&room_id);
    }

    /// The installed app handle (for emitting events from non-command paths).
    pub(crate) fn app_handle() -> Option<AppHandle> {
        APP.lock().unwrap().clone()
    }

    /// Install the live Gossip handle (called once the iroh endpoint is up).
    pub(crate) fn install(app: AppHandle, gossip: Gossip) {
        *APP.lock().unwrap() = Some(app);
        *GOSSIP.lock().unwrap() = Some(gossip);
        // Re-join every known room so we receive while the messenger is closed.
        for kv in DB.scan_prefix(b"chat:room:") {
            if let Ok((_, v)) = kv {
                if let Ok(r) = serde_json::from_slice::<super::Room>(&v) {
                    join(r.id);
                }
            }
        }
        spawn_self_heal();
        spawn_dht_gossip();
        spawn_outbox_flusher();
        super::sync::spawn_pull_loop();
        spawn_devhub_gossip();
        spawn_seeder_gossip();
        // Стелс: если режим включён (сохранён в БД) — сразу поднимаем фантомный трафик.
        if crate::stealth::is_stealth() {
            start_chaff();
        }
    }

    /// Self-healing mesh: every 30s re-inject the known bootstrap EndpointIds into
    /// each live topic so the gossip neighbor links re-form in BOTH directions.
    /// This fixes the owner→member and stale-direct delivery asymmetry behind
    /// NAT/relay (a node that only got dialed never dialed back, so its broadcasts
    /// never reached the other side). `join_peers` is idempotent — it no-ops on
    /// peers already connected and re-dials (over relay if needed) the rest.
    /// Also refreshes the on-chain EndpointId once if it drifted from the record.
    fn spawn_self_heal() {
        tauri::async_runtime::spawn(async move {
            // one-shot: refresh the on-chain EndpointId if it changed since publish
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            super::republish_endpoint_if_changed().await;
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            // счётчик подряд идущих циклов изоляции (0 соседей при живых бутстрапах)
            let mut isolated: HashMap<String, u32> = HashMap::new();
            loop {
                tick.tick().await;
                let entries: Vec<(String, iroh_gossip::api::GossipSender)> =
                    SENDERS.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (room_id, tx) in entries {
                    let room = match load_room(&room_id) {
                        Some(r) => r,
                        None => continue,
                    };
                    // Лечим только «живые» комнаты (активность ≤48ч): постоянные
                    // дозвоны до офлайн-пиров всех комнат штормят актор gossip
                    // (он один на все топики) и сами душат живые чаты.
                    let now = super::now_ms();
                    if now.saturating_sub(room.last_ts) > 48 * 3600 * 1000 {
                        isolated.remove(&room_id);
                        continue;
                    }
                    let boot_ids: Vec<String> = if room.kind == "group" {
                        super::group_boots(&room_id)
                    } else {
                        super::peer_node_id(&room.peer_address).into_iter().collect()
                    };
                    // v1.135.229: самые СВЕЖИЕ узлы (список упорядочен по давности,
                    // новые — в конце); раньше брались первые 3 — часто давно офлайн
                    let bootstrap: Vec<iroh::EndpointId> = boot_ids
                        .iter()
                        .rev()
                        .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
                        .take(3)
                        .collect();
                    if bootstrap.is_empty() {
                        continue;
                    }
                    if room_peer_count(&room_id) > 0 {
                        isolated.remove(&room_id);
                        // ЗОМБИ-детект: соседи «есть» (возможно, протухшие
                        // NeighborUp — при мёртвой подписке NeighborDown тоже не
                        // приходят), но событий с топика нет >5 минут. Раньше
                        // watchdog эскалировал ТОЛЬКО при 0 соседей — клиент с
                        // аптаймом 24ч показывал «ONLINE 12» и не получал ничего.
                        let last_rx = LAST_RX.lock().unwrap().get(&room_id).copied().unwrap_or(0);
                        if last_rx > 0 && now.saturating_sub(last_rx) > 5 * 60 * 1000 {
                            let ok = {
                                let mut lr = LAST_RESET.lock().unwrap();
                                let e = lr.entry(room_id.clone()).or_insert(0);
                                if now.saturating_sub(*e) > 10 * 60 * 1000 {
                                    *e = now;
                                    true
                                } else {
                                    false
                                }
                            };
                            if ok {
                                crate::syslog::log(
                                    "chat",
                                    format!(
                                        "mesh watchdog: no inbound gossip for {}s with {} neighbor(s) — zombie subscription, hard reset",
                                        now.saturating_sub(last_rx) / 1000,
                                        room_peer_count(&room_id)
                                    ),
                                );
                                LAST_RX.lock().unwrap().insert(room_id.clone(), now);
                                RESET.lock().unwrap().insert(room_id.clone());
                                RESET_NOTIFY.notify_waiters();
                                continue;
                            }
                        }
                    } else {
                        // 0 соседей при известных бутстрапах: 1-2 цикла лечим мягко
                        // (join_peers), с 3-го — жёсткий ресет подписки, но не чаще
                        // раза в 10 минут на комнату (иначе офлайн-лички devолвируют
                        // в бесконечный quit/join-шторм актора).
                        let n = isolated.entry(room_id.clone()).or_insert(0);
                        *n += 1;
                        if *n >= 3 {
                            let ok = {
                                let mut lr = LAST_RESET.lock().unwrap();
                                let e = lr.entry(room_id.clone()).or_insert(0);
                                if now.saturating_sub(*e) > 10 * 60 * 1000 {
                                    *e = now;
                                    true
                                } else {
                                    false
                                }
                            };
                            if ok {
                                // счётчик сбрасываем ТОЛЬКО когда ресет реально ушёл
                                *n = 0;
                                RESET.lock().unwrap().insert(room_id.clone());
                                RESET_NOTIFY.notify_waiters();
                                continue;
                            }
                            // кулдаун жёсткого ресета: НЕ пропускаем цикл — мягко
                            // лечим join_peers ниже (раньше `continue` оставлял комнату
                            // без единой попытки до 10 минут)
                        }
                    }
                    let _ = tx.join_peers(bootstrap).await;
                }
            }
        });
    }

    fn topic_of(room_id: &str) -> Option<TopicId> {
        let bytes = hex::decode(room_id).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(TopicId::from_bytes(arr))
    }

    /// Subscribe to a room's topic and pump inbound ciphertext through the gate.
    /// Direct chats bootstrap off the peer's on-chain EndpointId; groups bootstrap
    /// off the EndpointIds learned from invite links + `NeighborUp` events (groups
    /// have no on-chain identity to resolve).
    pub(crate) fn join(room_id: String) {
        let room = load_room(&room_id);
        let is_group = room.as_ref().map(|r| r.kind == "group").unwrap_or(false);
        let boots: Vec<String> = if is_group {
            super::group_boots(&room_id)
        } else {
            room.and_then(|r| super::peer_node_id(&r.peer_address)).into_iter().collect()
        };
        join_inner(room_id, boots);
    }

    /// Like `join` but the bootstrap is resolved from the peer ADDRESS directly —
    /// used for first-contact auto-subscribe (following) where no Room exists yet.
    pub(crate) fn join_for_peer(room_id: String, peer_addr: String) {
        join_inner(room_id, super::peer_node_id(&peer_addr).into_iter().collect());
    }

    fn join_inner(room_id: String, boot_ids: Vec<String>) {
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        // Явный join отменяет прежний leave.
        LEFT.lock().unwrap().remove(&room_id);
        let bootstrap: Vec<iroh::EndpointId> =
            boot_ids.iter().filter_map(|n| n.parse::<iroh::EndpointId>().ok()).collect();
        // Already subscribed: just (re)inject any bootstrap peers we have.
        if let Some(tx) = SENDERS.lock().unwrap().get(&room_id).cloned() {
            if !bootstrap.is_empty() {
                tauri::async_runtime::spawn(async move {
                    let _ = tx.join_peers(bootstrap).await;
                });
            }
            return;
        }
        // Цикл подписки уже крутится (переподписка после Lagged) — не дублируем.
        if !PUMPS.lock().unwrap().insert(room_id.clone()) {
            return;
        }
        let topic = match topic_of(&room_id) {
            Some(t) => t,
            None => {
                PUMPS.lock().unwrap().remove(&room_id);
                return;
            }
        };
        tauri::async_runtime::spawn(async move {
            let mut attempt: u32 = 0;
            loop {
                if LEFT.lock().unwrap().contains(&room_id) {
                    break;
                }
                // Свежие бутстрапы на каждую попытку: исходные + накопленные
                // NeighborUp'ами группы (add_group_boot) — самовосстановление меша.
                let mut ids: HashSet<String> = boot_ids.iter().cloned().collect();
                ids.extend(super::group_boots(&room_id));
                let boots: std::collections::BTreeSet<iroh::EndpointId> =
                    ids.iter().filter_map(|n| n.parse::<iroh::EndpointId>().ok()).collect();
                let opts = iroh_gossip::api::JoinOptions {
                    bootstrap: boots,
                    // Дефолт 2048: при переполнении iroh-gossip шлёт Lagged и
                    // ЗАКРЫВАЕТ подписчика. Расширяем буфер + ниже переподписка.
                    subscription_capacity: 8192,
                };
                let sub = match gossip.subscribe_with_opts(topic, opts).await {
                    Ok(s) => s,
                    Err(_) => {
                        attempt = attempt.saturating_add(1);
                        let delay = std::cmp::min(30u64, 1u64 << std::cmp::min(attempt, 5));
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        continue;
                    }
                };
                let (tx, rx) = sub.split();
                SENDERS.lock().unwrap().insert(room_id.clone(), tx);
                // grace-период зомби-детектора: отсчёт тишины с момента подписки
                LAST_RX.lock().unwrap().insert(room_id.clone(), super::now_ms());
                // подписка (пере)создана — дослать зависшие исходящие
                flush_outbox(&room_id);
                // Пересаб после возврата сети: последняя комната → меш готов,
                // сразу запускаем анти-энтропию (не ждём 45–75с цикла).
                let all_back = {
                    let mut rc = RECONNECTING.lock().unwrap();
                    rc.remove(&room_id) && rc.is_empty()
                };
                if all_back {
                    emit_mesh("ready", 0);
                    super::sync_now();
                    super::sync::pull_now();
                }
                let started = std::time::Instant::now();
                pump(room_id.clone(), rx).await;
                // Подписка закрылась: Lagged (переполнение буфера событий — iroh-gossip
                // закрывает подписчика НАСОВСЕМ) либо ошибка стрима. До v1.135.55 здесь
                // был тихий выход: приём умирал навсегда при живом tx в SENDERS —
                // «все шлют сообщения, но никто их не видит». Теперь: чистим мёртвый
                // tx и переподписываемся с экспоненциальным бэкоффом.
                SENDERS.lock().unwrap().remove(&room_id);
                // протухшие соседи мёртвой подписки: честный сброс счётчика
                // «ONLINE N» — заново наполнится NeighborUp'ами после переподписки
                PEERS.lock().unwrap().remove(&room_id);
                emit_peers(&room_id);
                if LEFT.lock().unwrap().contains(&room_id) {
                    break;
                }
                if started.elapsed() > std::time::Duration::from_secs(120) {
                    attempt = 0;
                }
                attempt = attempt.saturating_add(1);
                let delay = std::cmp::min(30u64, 1u64 << std::cmp::min(attempt, 5));
                crate::syslog::log(
                    "chat",
                    format!(
                        "gossip subscription closed (lagged/error), resubscribing in {}s",
                        delay
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
            PUMPS.lock().unwrap().remove(&room_id);
        });
    }

    async fn pump(room_id: String, mut rx: GossipReceiver) {
        use n0_future::StreamExt;
        loop {
            // Fast-path: notify_waiters не хранит «пермит» — если сигнал эскалации
            // пришёл, пока мы обрабатывали событие (не были в select), он бы
            // потерялся. Проверяем RESET на каждой итерации.
            if RESET.lock().unwrap().remove(&room_id) {
                crate::syslog::log("chat", "mesh watchdog: forcing full topic resubscribe");
                break;
            }
            // select: обычный поток событий ИЛИ сигнал watchdog-эскалации (полный
            // ресет подписки, когда join_peers не лечит изоляцию топика).
            let ev = tokio::select! {
                ev = rx.next() => match ev {
                    Some(e) => e,
                    None => break,
                },
                _ = RESET_NOTIFY.notified() => {
                    if RESET.lock().unwrap().remove(&room_id) {
                        crate::syslog::log("chat", "mesh watchdog: forcing full topic resubscribe");
                        break;
                    }
                    continue;
                }
            };
            // пища для зомби-детектора: любое событие = топик жив
            LAST_RX.lock().unwrap().insert(room_id.clone(), super::now_ms());
            match ev {
                Ok(Event::Received(m)) => {
                    // Стелс: снимаем padding / отсекаем фантомные (chaff) пакеты.
                    match crate::stealth::parse_incoming(&m.content) {
                        crate::stealth::Incoming::Chaff => continue,
                        crate::stealth::Incoming::Message(bytes) => {
                            let cipher = String::from_utf8_lossy(&bytes).to_string();
                            // НЕ ждём обработку (анти-спам делает блокирующий HTTP):
                            // медленный inbound копил очередь событий → Lagged →
                            // закрытие подписки. Спавним и читаем стрим дальше.
                            tauri::async_runtime::spawn(handle_inbound(room_id.clone(), cipher));
                        }
                    }
                }
                Ok(Event::NeighborUp(id)) => {
                    let nid = id.to_string();
                    PEERS.lock().unwrap().entry(room_id.clone()).or_default().insert(nid.clone());
                    emit_peers(&room_id);
                    // Remember group peers as future bootstraps (self-healing mesh).
                    if load_room(&room_id).map(|r| r.kind == "group").unwrap_or(false) {
                        super::add_group_boot(&room_id, &nid);
                    }
                    // появился живой сосед — дослать зависшие исходящие
                    flush_outbox(&room_id);
                }
                Ok(Event::NeighborDown(id)) => {
                    if let Some(set) = PEERS.lock().unwrap().get_mut(&room_id) {
                        set.remove(&id.to_string());
                    }
                    emit_peers(&room_id);
                }
                Ok(Event::Lagged) => {
                    crate::syslog::log("chat", "gossip receiver lagged — subscription will close");
                    continue;
                }
                Err(_) => break,
            }
        }
        // topic closed → clear presence
        PEERS.lock().unwrap().remove(&room_id);
        emit_peers(&room_id);
    }

    /// Decrypt + gate + store an inbound gossip packet. ALL work (anti-spam
    /// network lookup, DB, crypto) runs on the BLOCKING thread pool so the
    /// blocking `ureq` HTTP call never starves the async runtime (which froze
    /// the whole app — every window shares this runtime).
    async fn handle_inbound(room_id: String, cipher: String) {
        let _ = tauri::async_runtime::spawn_blocking(move || handle_inbound_blocking(&room_id, &cipher)).await;
    }

    /// true = сохранено НОВОЕ пользовательское сообщение (не дубликат, не sys).
    fn handle_inbound_blocking(room_id: &str, cipher: &str) -> bool {
        let app = match APP.lock().unwrap().clone() {
            Some(a) => a,
            None => return false,
        };
        // Lazily materialize the Room on first contact: if we auto-subscribed to a
        // followed peer's topic but have no Room yet, create it now from the
        // remembered topic→address mapping.
        let room = match load_room(room_id) {
            Some(r) => r,
            None => {
                let addr = super::FOLLOW_TOPICS.lock().unwrap().get(room_id).cloned();
                match addr.and_then(|a| super::ensure_room_for(&a).ok()) {
                    Some(r) => r,
                    None => return false,
                }
            }
        };
        let me = my_address().unwrap_or_default();

        // Group: membership = possessing the shared secret. Decrypt the envelope,
        // attribute it to its real sender, and skip our own echoed broadcasts.
        if room.kind == "group" {
            let key = match room_key(&room) {
                Ok(k) => k,
                Err(_) => return false,
            };
            if let Ok(plain) = decrypt(&key, cipher) {
                if let Ok(m) = serde_json::from_str::<ChatMsg>(&plain) {
                    if m.sender == me {
                        return false;
                    }
                    if let Some(kind) = m.sys.clone() {
                        if kind == "typing" {
                            // Эфемерный сигнал — только событие в UI, не храним.
                            emit_typing(room_id, &m.sender);
                            return false;
                        }
                        if kind == "ack" {
                            // квитанция доставки: text = id подтверждаемого сообщения
                            super::record_ack(&app, &room.id, &m.text, &m.sender);
                            return false;
                        }
                        if kind == "sync" {
                            // анти-энтропия: пир прислал список принятых id
                            super::handle_sync(&room, &m.text);
                            return false;
                        }
                        if kind == "react" {
                            // эфемерная реакция на сообщение — только стор + событие в UI
                            super::record_reaction(&app, &room.id, &m.text, &m.sender);
                            return false;
                        }
                        if kind == "edit" {
                            // автор заменил текст своего сообщения
                            super::record_edit(&app, &room.id, &m.text, &m.sender);
                            return false;
                        }
                        if kind == "policy" {
                            // Owner-signed policy update: verify + apply.
                            if let Ok(upd) = serde_json::from_str::<super::GroupPolicyUpdate>(&m.text) {
                                super::apply_policy_update(&app, &room, &upd);
                            }
                        } else if kind == "key_rotation" {
                            // Owner-broadcast group key rotation (newer version only).
                            super::apply_key_rotation(&room, &m.text);
                        } else {
                            let _ = super::store_sys(Some(&app), &room, &m.sender, &kind, "", false);
                        }
                        return false;
                    }
                    // Moderation: drop messages from kicked members.
                    if room.policy.as_ref().map(|p| p.banned.iter().any(|b| b == &m.sender)).unwrap_or(false) {
                        // раньше дроп был молчаливым — «пишет, но никто не видит»
                        // невозможно было отличить от проблем сети
                        crate::syslog::log(
                            "chat",
                            format!("group inbound DROPPED: sender {} is banned by group policy", &m.sender),
                        );
                        return false;
                    }
                    // Track the sender as a group member (key-rotation whitelist).
                    super::add_member(&room.id, &m.sender);
                    // Дедуп по id конверта: дубликат ресинка — только пере-квитанция.
                    if !super::mark_seen(&room.id, &m.id) {
                        super::send_ack(&room, &m.id);
                        return false;
                    }
                    store_inbound(&app, &room, &m.sender, &m.text, Some(&m.id), Some(m.ts));
                    // Квитанция доставки автору (E2EE, эфемерная — в историю не пишется).
                    super::send_ack(&room, &m.id);
                    return true;
                }
            } else {
                // ключ не подошёл: ротация не дошла / другая когорта — теперь видно в логах
                crate::syslog::log("chat", "group envelope decrypt failed (key mismatch?)");
            }
            return false;
        }

        let following = super::cached_following(&me);
        if !inbound_allowed(&room.peer_address, &following) {
            return false;
        }
        let key = match room_key(&room) {
            Ok(k) => k,
            Err(_) => return false,
        };
        if let Ok(plain) = decrypt(&key, cipher) {
            // Outbound envelope is a JSON ChatMsg; fall back to raw text.
            match serde_json::from_str::<ChatMsg>(&plain) {
                Ok(m) => {
                    // Собственный эхо-пакет из меша (как в group-ветке) — не
                    // сохраняем свои сообщения как входящие от собеседника.
                    if !m.sender.is_empty() && m.sender == me {
                        return false;
                    }
                    if m.sys.as_deref() == Some("typing") {
                        // Эфемерный «печатает…» (не наш собственный эхо-пакет).
                        emit_typing(room_id, &room.peer_address);
                        return false;
                    }
                    if m.sys.as_deref() == Some("ack") {
                        super::record_ack(&app, &room.id, &m.text, &room.peer_address);
                        return false;
                    }
                    if m.sys.as_deref() == Some("sync") {
                        super::handle_sync(&room, &m.text);
                        return false;
                    }
                    if m.sys.as_deref() == Some("react") {
                        super::record_reaction(&app, &room.id, &m.text, &room.peer_address);
                        return false;
                    }
                    if m.sys.as_deref() == Some("edit") {
                        super::record_edit(&app, &room.id, &m.text, &room.peer_address);
                        return false;
                    }
                    if m.sys.is_none() && !super::mark_seen(&room.id, &m.id) {
                        // дубликат анти-энтропийного ресинка — только пере-квитанция
                        super::send_ack(&room, &m.id);
                        return false;
                    }
                    store_inbound(&app, &room, &room.peer_address, &m.text, Some(&m.id), Some(m.ts));
                    if m.sys.is_none() {
                        super::send_ack(&room, &m.id);
                        return true;
                    }
                }
                Err(_) => store_inbound(&app, &room, &room.peer_address, &plain, None, None),
            }
        } else {
            crate::syslog::log("chat", "direct envelope decrypt failed (key mismatch?)");
        }
        false
    }

    /// Broadcast a free message's ciphertext onto its room topic (≤MAX_PACKET).
    pub(crate) fn broadcast(room_id: String, cipher: String) {
        if cipher.len() > MAX_PACKET {
            crate::syslog::log(
                "chat",
                format!("gossip drop: cipher {}B > {}B limit", cipher.len(), MAX_PACKET),
            );
            return;
        }
        join(room_id.clone());
        // Стелс: отмечаем реальную активность (сбрасывает окно chaff) и оборачиваем
        // payload в padding-фрейм (если стелс выключен — вернётся как есть).
        crate::stealth::note_activity();
        let payload = crate::stealth::frame_outbound(cipher.into_bytes());
        tauri::async_runtime::spawn(async move {
            let sender = SENDERS.lock().unwrap().get(&room_id).cloned();
            if let Some(tx) = sender {
                if let Err(e) = tx.broadcast(payload.clone().into()).await {
                    crate::syslog::log("chat", format!("gossip broadcast failed: {e} — queued to outbox"));
                    outbox_push(&room_id, payload);
                }
            } else {
                // подписка в процессе переподключения (Lagged-бэкофф) — пакет в outbox,
                // дошлётся при NeighborUp / после переподписки / фоновым флашером
                crate::syslog::log("chat", "gossip broadcast skipped: no live subscription — queued to outbox");
                outbox_push(&room_id, payload);
            }
        });
    }

    // ===== Фантомный трафик (chaff) — анти-DPI слой стелс-режима =====
    static CHAFF_RUNNING: AtomicBool = AtomicBool::new(false);

    /// Запустить фоновый генератор фантомного трафика (идемпотентно). В периоды
    /// простоя рассылает пустые (padding-only) фреймы по живым gossip-топикам со
    /// случайным интервалом 1.5–4.5 с → монотонный сетевой паттерн через реле.
    /// Таск сам завершается, когда стелс-режим выключается.
    pub(crate) fn start_chaff() {
        if CHAFF_RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        tauri::async_runtime::spawn(async move {
            loop {
                let delay = crate::stealth::next_chaff_delay_ms();
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if !crate::stealth::is_stealth() {
                    CHAFF_RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
                // Шлём фантомный трафик только в простое (не мешаем реальному).
                if !crate::stealth::idle() {
                    continue;
                }
                let senders: Vec<iroh_gossip::api::GossipSender> =
                    SENDERS.lock().unwrap().values().cloned().collect();
                if senders.is_empty() {
                    continue;
                }
                let frame = crate::stealth::frame_chaff();
                for tx in senders {
                    let f = frame.clone();
                    let _ = tx.broadcast(f.into()).await;
                }
            }
        });
    }

    // ===== Anti-censorship DHT-hint exchange (gossip fallback channel) =====
    // A well-known system topic where peers swap a FEW live Mainline DHT node
    // addrs (ip:port). Censored nodes (blocked from public routers/seeder, but
    // reachable on the mesh) listen here and re-bootstrap off the harvested
    // nodes. Hardened: tiny payload, infrequent broadcasts, and inbound merges
    // are rate-limited/validated/capped by `p2p::merge_dht_hints`.

    static DHT_TX: Lazy<Mutex<Option<iroh_gossip::api::GossipSender>>> =
        Lazy::new(|| Mutex::new(None));

    fn dht_topic() -> TopicId {
        // 32-byte nothing-up-my-sleeve constant (exactly 32 ASCII chars).
        TopicId::from_bytes(*b"SMARTNET-DHT-HINTS-BOOTSTRAP-v01")
    }

    fn join_dht_topic() {
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        if DHT_TX.lock().unwrap().is_some() {
            return;
        }
        let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
            .iter()
            .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
            .collect();
        let topic = dht_topic();
        tauri::async_runtime::spawn(async move {
            let sub = match gossip.subscribe(topic, boots).await {
                Ok(s) => s,
                Err(_) => return,
            };
            let (tx, mut rx) = sub.split();
            *DHT_TX.lock().unwrap() = Some(tx);
            use n0_future::StreamExt;
            while let Some(ev) = rx.next().await {
                if let Ok(Event::Received(m)) = ev {
                    if m.content.len() <= 2048 {
                        if let Ok(nodes) = serde_json::from_slice::<Vec<String>>(&m.content) {
                            tauri::async_runtime::spawn_blocking(move || {
                                crate::p2p::merge_dht_hints(nodes);
                            });
                        }
                    }
                }
            }
            *DHT_TX.lock().unwrap() = None;
        });
    }

    /// Periodically keep the DHT-hint overlay alive and (only when our own DHT is
    /// healthy) share a small sample of our live nodes for censored peers.
    fn spawn_dht_gossip() {
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(120));
            loop {
                tick.tick().await;
                join_dht_topic();
                let tx = DHT_TX.lock().unwrap().clone();
                if let Some(tx) = tx {
                    // re-inject known peers so the overlay re-forms (idempotent)
                    let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
                        .iter()
                        .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
                        .collect();
                    if !boots.is_empty() {
                        let _ = tx.join_peers(boots).await;
                    }
                    // Only nodes WITH a healthy DHT advertise (≥8); censored nodes listen.
                    let healthy = crate::p2p::DHT_NODES
                        .load(std::sync::atomic::Ordering::Relaxed)
                        >= 8;
                    let sample = crate::p2p::dht_share_sample();
                    if healthy && !sample.is_empty() {
                        if let Ok(payload) = serde_json::to_vec(&sample) {
                            let _ = tx.broadcast(payload.into()).await;
                        }
                    }
                }
            }
        });
    }

    // ===== DevHub-анонсы: gossip-шина обнаружения авторов dev:// =====
    // NAT-proof канал: UDP-бикон за NAT снаружи недостижим, а записи Mainline
    // DHT протухают за ~15–30 минут. Все клиенты подписаны на общий системный
    // топик; авторы периодически бродкастят компактный анонс (кто я, мой
    // EndpointId, хэш подписанного индекса, seq). Анонс — только подсказка:
    // сам индекс верифицируется подписью кошелька в store_remote_index.

    static DEVHUB_TX: Lazy<Mutex<Option<iroh_gossip::api::GossipSender>>> =
        Lazy::new(|| Mutex::new(None));

    fn devhub_topic() -> TopicId {
        // 32-байтовая nothing-up-my-sleeve константа (ровно 32 ASCII-символа).
        TopicId::from_bytes(*b"SMARTNET-DEVHUB-ANNOUNCE-BUS-v01")
    }

    fn join_devhub_topic() {
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        if DEVHUB_TX.lock().unwrap().is_some() {
            return;
        }
        let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
            .iter()
            .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
            .collect();
        let topic = devhub_topic();
        tauri::async_runtime::spawn(async move {
            let sub = match gossip.subscribe(topic, boots).await {
                Ok(s) => s,
                Err(_) => return,
            };
            let (tx, mut rx) = sub.split();
            *DEVHUB_TX.lock().unwrap() = Some(tx);
            use n0_future::StreamExt;
            while let Some(ev) = rx.next().await {
                if let Ok(Event::Received(m)) = ev {
                    if m.content.len() <= 1024 {
                        let bytes = m.content.to_vec();
                        tauri::async_runtime::spawn_blocking(move || {
                            crate::devhub::handle_gossip_announce(&bytes);
                        });
                    }
                }
            }
            *DEVHUB_TX.lock().unwrap() = None;
        });
    }

    /// Немедленный бродкаст своего DevHub-анонса (вызывается из publish_index
    /// после каждого подписанного изменения — соседи узнают о репо мгновенно).
    pub(crate) fn devhub_announce_now() {
        let Some(app) = app_handle() else { return };
        join_devhub_topic();
        tauri::async_runtime::spawn(async move {
            if let Some(payload) = crate::devhub::gossip_announce_payload(&app).await {
                let tx = DEVHUB_TX.lock().unwrap().clone();
                if let Some(tx) = tx {
                    let _ = tx.broadcast(payload.into()).await;
                }
            }
        });
    }

    /// Бродкаст произвольного payload'а в системный DevHub/альбомный топик
    /// (используется social.rs для мгновенного анонса keyfile'ов альбомов).
    pub(crate) fn broadcast_on_devhub_topic(payload: Vec<u8>) {
        join_devhub_topic();
        tauri::async_runtime::spawn(async move {
            let tx = DEVHUB_TX.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx.broadcast(payload.into()).await;
            }
        });
    }

    /// Держим DevHub-оверлей живым: каждые 2 мин ре-джойн + бродкаст анонса.
    /// Стартовая пауза 35 с — даём publish_index при старте засидировать индекс
    /// (иначе первый анонс уйдёт без хэша blob'а).
    fn spawn_devhub_gossip() {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(35)).await;
            loop {
                join_devhub_topic();
                let tx = DEVHUB_TX.lock().unwrap().clone();
                if let Some(tx) = tx {
                    // ре-инъекция bootstrap-пиров: оверлей пересобирается (идемпотентно)
                    let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
                        .iter()
                        .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
                        .collect();
                    if !boots.is_empty() {
                        let _ = tx.join_peers(boots).await;
                    }
                    if let Some(app) = app_handle() {
                        if let Some(payload) = crate::devhub::gossip_announce_payload(&app).await {
                            let _ = tx.broadcast(payload.into()).await;
                        }
                        // Анонсы keyfile'ов моих платных альбомов (NAT-proof).
                        for p in crate::social::album_gossip_payloads(&app).await {
                            let _ = tx.broadcast(p.into()).await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            }
        });
    }

    // ===== Cloud Seeder анонсы: авто-обмен seeder-URI между живыми пирами =====
    // Новый сидер после публикации бродкастит свой подписанный URI в общий
    // системный топик; получатели проверяют ECDSA-подпись pubkey из URI и
    // вливают его в свой реестр → при их следующей публикации URI уходит и в
    // их seeders.enc в облаке (цепная реакция размазывает сеть саму себя).

    static SEEDER_TX: Lazy<Mutex<Option<iroh_gossip::api::GossipSender>>> =
        Lazy::new(|| Mutex::new(None));

    fn seeder_topic() -> TopicId {
        // 32-байтовая nothing-up-my-sleeve константа (ровно 32 ASCII-символа).
        TopicId::from_bytes(*b"SMARTNET-SEEDER-ANNOUNCE-BUS-v01")
    }

    fn join_seeder_topic() {
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        if SEEDER_TX.lock().unwrap().is_some() {
            return;
        }
        let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
            .iter()
            .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
            .collect();
        let topic = seeder_topic();
        tauri::async_runtime::spawn(async move {
            let sub = match gossip.subscribe(topic, boots).await {
                Ok(s) => s,
                Err(_) => return,
            };
            let (tx, mut rx) = sub.split();
            *SEEDER_TX.lock().unwrap() = Some(tx);
            use n0_future::StreamExt;
            while let Some(ev) = rx.next().await {
                if let Ok(Event::Received(m)) = ev {
                    if m.content.len() <= 2048 {
                        let bytes = m.content.to_vec();
                        tauri::async_runtime::spawn_blocking(move || {
                            crate::cloudseeder::handle_gossip_announce(&bytes);
                        });
                    }
                }
            }
            *SEEDER_TX.lock().unwrap() = None;
        });
    }

    /// Немедленный бродкаст своего seeder-URI (вызывается после каждой
    /// успешной публикации /ntfryseed — соседи узнают о сидере мгновенно).
    pub(crate) fn seeder_announce_now() {
        join_seeder_topic();
        tauri::async_runtime::spawn(async move {
            let payloads = tauri::async_runtime::spawn_blocking(
                crate::cloudseeder::gossip_announce_payloads,
            )
            .await
            .unwrap_or_default();
            if !payloads.is_empty() {
                let tx = SEEDER_TX.lock().unwrap().clone();
                if let Some(tx) = tx {
                    // primary + secondary-зеркало: сеть знает оба URI
                    for payload in payloads {
                        let _ = tx.broadcast(payload.into()).await;
                    }
                }
            }
        });
    }

    /// Держим seeder-оверлей живым: каждые 5 мин ре-джойн + анонс своего URI
    /// (если публиковались). Стартовая пауза 40 с — ждём iroh-бутстрап.
    fn spawn_seeder_gossip() {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(40)).await;
            loop {
                join_seeder_topic();
                let tx = SEEDER_TX.lock().unwrap().clone();
                if let Some(tx) = tx {
                    let boots: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
                        .iter()
                        .filter_map(|n| n.parse::<iroh::EndpointId>().ok())
                        .collect();
                    if !boots.is_empty() {
                        let _ = tx.join_peers(boots).await;
                    }
                    let payloads = tauri::async_runtime::spawn_blocking(
                        crate::cloudseeder::gossip_announce_payloads,
                    )
                    .await
                    .unwrap_or_default();
                    for payload in payloads {
                        let _ = tx.broadcast(payload.into()).await;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            }
        });
    }
