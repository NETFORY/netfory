// ============================================================
//  SmartNet // netstat.rs — глобальный сэмплер сетевого трафика
//
//  Раз в секунду суммирует rx/tx по всем НЕ-loopback интерфейсам ОС
//  (sysinfo) и эмитит `net-speed` { rxBps, txBps } во фронтенд.
//  sysinfo отдаёт байты С МОМЕНТА прошлого refresh — при шаге 1 с
//  значения и есть байт/сек. Покрывает ВЕСЬ трафик машины (торренты,
//  iroh QUIC, DHT, api://, clearnet), а не только librqbit.
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use sysinfo::Networks;
use tauri::Emitter;

pub(crate) fn start(app: tauri::AppHandle) {
    let _ = std::thread::Builder::new().name("netstat".into()).spawn(move || {
        let mut networks = Networks::new_with_refreshed_list();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            networks.refresh(true);
            let (mut rx, mut tx) = (0u64, 0u64);
            for (name, data) in networks.iter() {
                let n = name.to_ascii_lowercase();
                // Пропускаем loopback: локальные стримы (torrent stream server)
                // не являются «сетью» и задваивали бы цифры.
                if n == "lo" || n.starts_with("loopback") {
                    continue;
                }
                rx = rx.saturating_add(data.received());
                tx = tx.saturating_add(data.transmitted());
            }
            let _ = app.emit("net-speed", serde_json::json!({ "rxBps": rx, "txBps": tx }));
        }
    });
}
