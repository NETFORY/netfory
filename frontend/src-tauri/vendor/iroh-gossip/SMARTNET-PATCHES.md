# iroh-gossip 0.101.0 - патчи SmartNet (из smartholdem/sth-core-pq, 2026-10-10)

Исходник: crates.io `iroh-gossip 0.101.0` (без examples/tests/bin). Изменены:

| Файл | Патч |
|---|---|
| `src/net/util.rs` | `SEND_TIMEOUT = 10с` на запись сообщения пиру (`WriteError::SlowPeer`); `recv()` -> `None` завершает send-loop, когда актор удалил пира (**n0-computer/iroh-gossip#145**: раньше ветка молча отключалась и QUIC-соединение + задача текли на каждого ротированного пира). |
| `src/net.rs` | Актор не ждёт очередь одного пира (`try_send`; полная очередь -> сообщение пиру дропается, plumtree IHave/Graft восстановит); завершение send-loop закрывает соединение, чтобы recv-loop тоже завершился и актор получил `PeerDisconnected`. |
| `src/proto/state.rs` | `peer_topics` очищается при `PeerDisconnected` (#145); соединение не закрывается, пока его использует другой топик (**#172**). |

Подключено через `[patch.crates-io]` в `frontend/src-tauri/Cargo.toml` и `messenger/src-tauri/Cargo.toml`.
`netfory-provider` пока использует crates.io-версию (отдельный репозиторий/деплой).
