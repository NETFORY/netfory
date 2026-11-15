# mycelium - mushroom-маршрутизация NETFORY (NIP-1)

Библиотека + консольный узел `myceliumd`. Спецификация: `../docs/NIP-1-Mycelium.md`, план: `../Mycelium-Plan.md`.

## Библиотека
```rust
let my = mycelium::Mycelium::start(endpoint.clone(), Some(gossip), mycelium::Config::default(), known_peers).await?;
router.accept(mycelium::ALPN, my.handler());   // релей (принимает цепочки только при set_relay_enabled(true))
my.set_enabled(true);                          // клиент: пул цепочек строится в фоне
let resp = my.open_stream(target, b"netfory/chat-dm/2").await?.request(&payload, 64 * 1024).await?;
```

## myceliumd
```bash
cargo build --release --features daemon --bin myceliumd
./myceliumd init                    # создать myceliumd.toml по умолчанию
./myceliumd relay                   # только релей
./myceliumd socks                   # релей (role = "relay") + SOCKS5 127.0.0.1:9150
./myceliumd --config /etc/myceliumd.toml socks
```
SOCKS5: хост `<EndpointId>.iroh`, порт → ALPN по таблице `[ports]` в конфиге (`7000 = "netfory/chat-dm/2"` и т.д.).
Netfory на той же машине видит демон (Настройки → Сеть → Mycelium: «Системный Mycelium запущен») и не поднимает свой релей.

## Релеи NETFORY
Серверный вариант - не отдельный `myceliumd`, а `pq-web serve --mycelium` (`../pq/pq-web/README.md`):
тот же EndpointId, анонс на трекер с флагом `mycelium`, клиенты берут узел в bootstrap сами.

## Пересборка цепочки
- `probe_hops` помечает хоп мёртвым без PONG дольше `HOP_DEAD_AFTER` (10 с); `CircuitInfo.hop_alive`,
  `degraded` (мёртвый хоп или участок > `SEGMENT_SLOW_MS` = 800 мс), `retries`, `rebuilding`.
- Обслуживание (раз в 30 с) само пересобирает деградировавшие цепочки до `AUTO_REBUILD_MAX` = 3 раз
  подряд (`rebuild(id, auto=true)`), здоровая цепочка старше 3 мин сбрасывает счётчик.
- `Mycelium::rebuild(id, false)` - ручная кнопка «Построить новую цепочку» на карте: мёртвые хопы
  исключаются (и штрафуются в директории), у медленной живой цепочки меняются Middle/Exit (guard
  закреплён), замена встаёт в тот же слот, старая закрывается только когда новая готова.

## Фаза 2: скрытый приём и маскирующий трафик
`Config.hidden` (по умолч. true) - при `set_service_handler(...)` + `set_enabled(true)` клиент поднимает
`intro_points` intro-цепочек, публикует `ServiceDescriptor` в gossip и принимает INTRODUCE2 →
`RENDEZVOUS1`. `open_stream(target)` сам выбирает путь: описатель адресата известен → точка встречи
(`CircuitInfo.kind = "rendezvous"`), иначе Exit. `set_cover_traffic(true)` - PADDING на всех цепочках.
Спецификация - `../docs/NIP-1-Mycelium.md` §7, тест `rendezvous_hidden_service`.

## Фаза 3: системный VPN (`mycelium-tun`)
```bash
cargo build --release --features tun --bin mycelium-tun
mycelium-tun --socks 127.0.0.1:PORT --ctl 127.0.0.1:PORT --token HEX [--bypass ip,ip,...] [--log FILE]
```
Привилегированный помощник: TUN (tun2proxy) → SOCKS5 Netfory → цепочка → Exit → pq-web `--open` → интернет.
`socks::serve(..., Some(egress))` принимает любые хосты и отдаёт не-`.iroh` в `Egress`; `Mycelium::bypass_addrs()`
- адреса guard-релеев и реле iroh, которые туннель должен пускать мимо себя. Windows: рядом нужен `wintun.dll`.
Спецификация - `../docs/NIP-1-Mycelium.md` §8.

## Тесты
```bash
cargo test --features daemon --test mushroom   # 8 сквозных сценариев на локальных iroh-узлах
```

## Устойчивость (1.136.2)
Адаптивная длина цепочки при нехватке релеев + автоапгрейд, пересборка мёртвых хопов без лимита
(backoff до 160 с), забывание сбоев через 5 мин, адресные подсказки `RelayDescriptor.addrs` /
`Mycelium::add_hints`, ключ личности `Mycelium::set_identity` (отдельный транспортный endpoint).
Исправлена вечная блокировка в `status()`/`circuits()` - pq-web c `--mycelium` обновить до 0.6.1.
