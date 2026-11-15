# NETFORY Messenger (`ntfry-messenger`)

Отдельное Tauri-приложение мессенджера NETFORY. Работает как **сайдкар** Netfory (desktop)
и как **standalone**-программа. 

## Структура
```
messenger/
  package.json, vite.config.js      UI: Vite + Vue; '@' -> ../frontend/src (ChatWidget, bridge, i18n, стили - без копий)
  ui/                                main.ts, App.vue (titlebar, вход, миграция), messenger.css
  src-tauri/
    Cargo.toml, tauri.conf.json      бинарь ntfry-messenger, identifier io.smartholdem.netfory.messenger
    src/lib.rs                       режимы, сессия (сид в RAM), своя sled-база, команды msg_*
    src/ipc.rs                       JSONL stdin/stdout ⇄ Netfory (типы в crates/netfory-chat-core)
    src/p2p.rs                       свой iroh-endpoint (ключ из сида), gossip, blobs, chat-dm/1,2
    src/chat/                        движок чата - копия frontend/src-tauri/src/chat (E2EE, комнаты, медиа, баны)
    src/shims.rs                     тонкие шимы вместо модулей монолита (naming по IPC, sth_node "скоро", ...)
    src/crypto.rs, stealth.rs, syslog.rs
crates/netfory-chat-core/            identity (сид -> xkey / EndpointId мессенджера), ipc-протокол, host-трейты
```

## Идентичность и данные
- Чат-ключ X25519 (`xkey`) выводится **так же, как в Netfory** (`sha256("smartnet-x25519-v1"+seed)`) -
  существующие контакты и комнаты продолжают работать.
- Транспортный EndpointId - новый: `sha256("netfory-messenger-endpoint|v1|"+seed)`; публикуется
  чат-движком при первом старте (publish_chat_key).
- Данные: `<app_data>/io.smartholdem.netfory.messenger/data/{messenger_db, blobs, messenger/}`
  (переопределяется env `NTFRY_MESSENGER_DATA`). Общего состояния с Netfory на диске нет.

## Режимы
- **Сайдкар.** Netfory (`messenger_launcher.rs`) запускает бинарь рядом со своим exe
  (`ntfry-messenger[.exe]`, либо env `NTFRY_MESSENGER_BIN`, либо PATH), пишет в stdin `hello{seed,address,lang}`;
  из stdout читает `unread/new_message/resolve/pay_request/open_in_host/export_request`.
  Повторный клик "Мессенджер" -> `focus`. Закрытие сессии Netfory -> `shutdown`.
  Если бинаря нет - Netfory показывает встроенный чат (без регрессии).
- **Standalone.** За 1.5 с не пришёл `hello` -> экран входа: сид-фраза вручную, либо payload QR из
  Netfory -> Ключи (`ntfry://seed/v1?...`) + PIN, либо кнопка **"Сканировать QR"** - камера через
  `getUserMedia` (`ui/QrScanner.vue`: нативный `BarcodeDetector`, иначе `jsqr`), кадры не покидают
  устройство. Окно `main` создаётся в `setup` (`"create": false` в конфиге), чтобы повесить
  `on_permission_request` -> камера разрешена. Резолв имён работает только из кэша (нет Netfory),
  платежи - "скоро" (этап 2).

## Миграция
Кнопка "Перенести" в мессенджере -> `open_in_host netfory://messenger/export` -> Netfory
`chat_export_archive` (sled-ключи `chat:/xkey:/group:/msg:/ban:/direct:/mcost:` в base64) ->
stdin `archive{json}` -> `msg_import_archive`. Однократно (`cfg:migrated_from_netfory`), повтор -
из настроек, пока Netfory хранит архив.

## Сборка
```bash
cd messenger && yarn && yarn tauri build        # или yarn tauri dev (UI на :3010)
# результат положить рядом с Netfory.exe (или добавить в bundle.externalBin Netfory,
# когда CI будет собирать бинарь с target-triple суффиксом)
```
Требуется то же дерево репозитория: `[patch.crates-io] iroh = ../../frontend/src-tauri/vendor/iroh`.

### Android
```bash
cd messenger && yarn
yarn android:init            # tauri android init -> scripts/android-setup.cjs (разрешения + CallService + MainActivity)
yarn android:icons           # mipmap-иконки из src-tauri/icons/icon.png (после init)
yarn android:dev             # устройство по USB: adb reverse 3010/3011, Vite на 127.0.0.1 (Wi-Fi/брандмауэр не нужны)
yarn android:dev:lan         # прежний режим: dev-сервер на публичном IP машины (TAURI_DEV_HOST), HMR :3011
yarn android:build           # APK (все ABI)   | yarn android:build:arm64 - только aarch64
yarn android:build:aab       # AAB для Google Play
yarn android:setup           # повторно донастроить gen/android (идемпотентно)
```
Аудио на Android: cpal -> oboe (C++). Линкуем общий libc++ (`cpal/oboe-shared-stdcxx` под
`cfg(target_os="android")`), а `scripts/android.cjs` перед сборкой копирует `libc++_shared.so` из
NDK (`toolchains/llvm/prebuilt/*/sysroot/usr/lib/<triple>/`) в `app/src/main/jniLibs/<abi>/` -
без этого APK падает на старте: `UnsatisfiedLinkError: cannot locate symbol "__cxa_pure_virtual"`.
Всё на Node - работает в Windows 11 без bash/WSL. `scripts/android.cjs` перед `tauri android ...`
готовит окружение для нативных зависимостей: cmake/ninja из `<ANDROID_HOME>/cmake/<ver>/bin`
(если нет в PATH), `CMAKE_GENERATOR=Ninja`, `ANDROID_NDK_ROOT`/`NDK_HOME` (из env или
`<ANDROID_HOME>/ndk/<ver>`), `CMAKE_POLICY_VERSION_MINIMUM=3.5` - без этого `audiopus_sys`
(libopus) падает "is cmake not installed?" / выбирает Visual Studio-генератор. Нужны: Android
Studio + SDK 34, SDK Tools -> NDK (Side by side) и CMake, rustup targets `aarch64-linux-android
armv7-linux-androideabi x86_64-linux-android`, `ANDROID_HOME`.

Фон (`android/CallService.kt`, `android/MainActivity.kt` -> копируются в gen/android): foreground
service типа `phoneCall|microphone` с partial wake lock держит процесс при сворачивании - P2P
(iroh) и активный звонок не обрываются; уведомление канала IMPORTANCE_MIN (без звука/иконки в
статус-баре); при первом запуске запрашивается исключение из оптимизации батареи (Doze режет сеть).
После выдачи RECORD_AUDIO сервис перезапускается с типом microphone (иначе Android 14 не даст
микрофон в фоне). Системная "назад" -> `window.__ntfryBack()` в ChatWidget (закрыть модалку /
вернуться к списку чатов), иначе приложение сворачивается (`moveTaskToBack`), не закрывается.

Входящий звонок на заблокированном телефоне: Rust (`chat/voice.rs` -> `android.rs`, JNI) вызывает
`CallService.showIncoming` - уведомление канала "Входящие звонки" (IMPORTANCE_HIGH, рингтон,
вибрация) с `fullScreenIntent` -> `IncomingCallActivity` поверх блокировки ("Ответить"/"Отклонить");
те же кнопки в самом уведомлении. Действие приходит в MainActivity (`ntfry_call`) -> JS
`ntfry-call-action` -> `voice_answer`. `hideIncoming` - при ответе/отклонении/таймауте. Android 14+:
для приложений из Play разрешение USE_FULL_SCREEN_INTENT включает пользователь - при первом
запуске открываем нужный экран настроек (sideload - выдано сразу).

Telecom (`android/NtfryConnectionService.kt`, self-managed ConnectionService, API 26+): каждый звонок
регистрируется в системном Telecom - ОС сама маршрутизирует звук (Bluetooth SCO / проводная
гарнитура / громкая связь / разговорный динамик), кнопки гарнитуры "ответить/завершить" приходят
как onAnswer/onDisconnect -> JS (`ntfry-call-action`: answer/decline/hangup/mute/unmute), сотовый
звонок корректно ставит наш на удержание (микрофон глушится). Состояния идут из Rust
(`voice.rs emit_state` -> `android.rs` -> `CallService.callState`): calling -> placeCall, active ->
setActive, ended -> setDisconnected(причина). Входящий - `addNewIncomingCall` -> onShowIncomingCallUi
-> наше полноэкранное уведомление; если Telecom отказал (`isIncomingCallPermitted=false`) -
уведомление напрямую. В баре звонка на Android - кнопка "Громкая связь" (`voice_audio_route`).
Свайп вправо по переписке (≥90px, быстрый, горизонтальный) -> список чатов.

Подпись APK/AAB: шаблон tauri сам `keystore.properties` не читает (поэтому
`app-universal-release-unsigned.apk`). `yarn android:setup` добавляет в `app/build.gradle.kts`
`signingConfigs.release` из `src-tauri/gen/android/keystore.properties`:
```
storePassword=...
keyPassword=...
keyAlias=ntfrychat
storeFile=../keys/ntfrychat.jks     # путь к ФАЙЛУ keystore, не к папке; слэши прямые
```
Создать ключ: `keytool -genkey -v -keystore ntfrychat.jks -keyalg RSA -keysize 2048 -validity 10000 -alias ntfrychat`.
Без файла keystore.properties release собирается как раньше (unsigned).
`android:build`/`android:dev` сами запускают `android-setup.cjs` (патч gradle + диагностика
keystore.properties: пустые пароли, storeFile-папка/не найден -> предупреждение "ПОДПИСЬ НЕ БУДЕТ
ПРИМЕНЕНА"). Проверить подпись готового APK:
```
"%ANDROID_HOME%\build-tools\34.0.0\apksigner.bat" verify --print-certs app-arm64-release.apk
keytool -printcert -jarfile app-arm64-release.apk
```
Подписанный файл называется `app-<abi>-release.apk` (без `-unsigned`).

Версия APK: единственный источник - `package.json` -> `scripts/sync-version.cjs` переписывает
`tauri.conf.json` (`version`, `bundle.android.versionCode = major·1e6 + minor·1e3 + patch`) и
`Cargo.toml`; запускается автоматически перед `android:*` и `build:*`, вручную - `yarn version:sync`.
Поднять релиз: `yarn version patch` (или правка version в package.json) -> `yarn android:build`.

Звонки: перед первым звонком UI вызывает `getUserMedia({audio})` - WebView (wry) запрашивает
системное RECORD_AUDIO, `on_permission_request(Microphone->Allow)` в `lib.rs` подтверждает, дальше
cpal/oboe. Уведомления о сообщениях/звонках - `tauri-plugin-notification`. Мобильный layout
(≤760px): список чатов и переписка - отдельные экраны, кнопка "‹" в шапке переписки.
