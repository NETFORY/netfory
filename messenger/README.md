# NETFORY Messenger (`ntfry-messenger`)

A separate Tauri-based NETFORY messenger application. It works both as a **sidecar** for Netfory (desktop)
and as a **standalone** application.

## Structure
```
messenger/
  package.json, vite.config.js      UI: Vite + Vue; '@' -> ../frontend/src (ChatWidget, bridge, i18n, styles - without copies)
  ui/                                main.ts, App.vue (titlebar, login, migration), messenger.css
  src-tauri/
    Cargo.toml, tauri.conf.json      ntfry-messenger binary, identifier io.smartholdem.netfory.messenger
    src/lib.rs                       modes, session (seed in RAM), its own sled database, msg_* commands
    src/ipc.rs                       JSONL stdin/stdout ⇄ Netfory (types in crates/netfory-chat-core)
    src/p2p.rs                       its own iroh endpoint (key from seed), gossip, blobs, chat-dm/1,2
    src/chat/                        chat engine - copy of frontend/src-tauri/src/chat (E2EE, rooms, media, bans)
    src/shims.rs                     thin shims instead of monolith modules (naming via IPC, sth_node "soon", ...)
    src/crypto.rs, stealth.rs, syslog.rs
crates/netfory-chat-core/            identity (seed -> xkey / messenger EndpointId), IPC protocol, host traits
```

## Identity and data
- The X25519 chat key (`xkey`) is derived **the same way as in Netfory** (`sha256("smartnet-x25519-v1"+seed)`), so
  existing contacts and rooms continue to work.
- The transport EndpointId is new: `sha256("netfory-messenger-endpoint|v1|"+seed)`; it is published by the
  chat engine on first startup (`publish_chat_key`).
- Data location: `<app_data>/io.smartholdem.netfory.messenger/data/{messenger_db, blobs, messenger/}`
  (overridden by `NTFRY_MESSENGER_DATA`). There is no shared state with Netfory on disk.

## Modes
- **Sidecar.** Netfory (`messenger_launcher.rs`) launches the binary next to its own exe
  (`ntfry-messenger[.exe]`, or `NTFRY_MESSENGER_BIN`, or PATH), writes `hello{seed,address,lang}` to stdin,
  and reads `unread/new_message/resolve/pay_request/open_in_host/export_request` from stdout.
  Clicking "Messenger" again triggers `focus`. Closing the Netfory session triggers `shutdown`.
  If the binary is missing, Netfory displays the built-in chat (without regression).
- **Standalone.** If no `hello` arrives within 1.5s, the login screen appears: seed phrase manually, or a QR payload from
  Netfory -> keys (`ntfry://seed/v1?...`) + PIN, or the **"Scan QR"** button - the camera via
  `getUserMedia` (`ui/QrScanner.vue`: native `BarcodeDetector`, otherwise `jsqr`), with frames never leaving
  the device. The `main` window is created in `setup` (`"create": false` in the config) so that
  `on_permission_request` can be attached once camera permission is granted. Name resolution works only from cache (no Netfory),
  and payments are "coming soon" (stage 2).

## Migration
The "Transfer" button in the messenger -> `open_in_host netfory://messenger/export` -> Netfory
`chat_export_archive` (sled keys `chat:/xkey:/group:/msg:/ban:/direct:/mcost:` in base64) ->
stdin `archive{json}` -> `msg_import_archive`. One-time migration (`cfg:migrated_from_netfory`), repeated from settings while Netfory still keeps the archive.

## Build
```bash
cd messenger && yarn && yarn tauri build        # or yarn tauri dev (UI on :3010)
# place the result next to Netfory.exe (or add bundle.externalBin Netfory,
# when CI starts building the binary with the target-triple suffix)
```
The same repository tree is required: `[patch.crates-io] iroh = ../../frontend/src-tauri/vendor/iroh`.

### Android
```bash
cd messenger && yarn
yarn android:init            # tauri android init -> scripts/android-setup.cjs (permissions + CallService + MainActivity)
yarn android:icons           # mipmap icons from src-tauri/icons/icon.png (after init)
yarn android:dev             # USB device: adb reverse 3010/3011, Vite on 127.0.0.1 (Wi‑Fi/firewall not needed)
yarn android:dev:lan         # previous mode: dev server on public IP (TAURI_DEV_HOST), HMR :3011
yarn android:build           # APK (all ABIs)   | yarn android:build:arm64 - only aarch64
yarn android:build:aab       # AAB for Google Play
yarn android:setup           # reconfigure gen/android idempotently
```
Android audio: cpal -> oboe (C++). We link the common libc++ (`cpal/oboe-shared-stdcxx` under
`cfg(target_os="android")`), and `scripts/android.cjs` copies `libc++_shared.so` from the
NDK (`toolchains/llvm/prebuilt/*/sysroot/usr/lib/<triple>/`) into `app/src/main/jniLibs/<abi>/` before the build -
without this, the APK crashes at startup: `UnsatisfiedLinkError: cannot locate symbol "__cxa_pure_virtual"`.
Everything is Node-based and works on Windows 11 without bash/WSL. `scripts/android.cjs` prepares the environment for native dependencies before `tauri android ...`:
cmake/ninja from `<ANDROID_HOME>/cmake/<ver>/bin` (if not in PATH), `CMAKE_GENERATOR=Ninja`, `ANDROID_NDK_ROOT`/`NDK_HOME` (from env or
`<ANDROID_HOME>/ndk/<ver>`), `CMAKE_POLICY_VERSION_MINIMUM=3.5` - without this, `audiopus_sys`
(libopus) fails with "is cmake not installed?" or picks the Visual Studio generator. Required tools: Android
Studio + SDK 34, SDK Tools -> NDK (Side by side) and CMake, rustup targets `aarch64-linux-android
armv7-linux-androideabi x86_64-linux-android`, `ANDROID_HOME`.

Background service (`android/CallService.kt`, `android/MainActivity.kt` -> copied into gen/android): a foreground
service of type `phoneCall|microphone` with a partial wake lock keeps the process alive while minimized - P2P
(iroh) and active calls remain connected; notification channel `IMPORTANCE_MIN` (no sound/no icon in the status bar); on first launch, battery optimization is exempted (Doze reduces network access).
After RECORD_AUDIO is granted, the service restarts with the `microphone` type (otherwise Android 14 will not allow microphone access in the background). The system's Back button -> `window.__ntfryBack()` in ChatWidget (close modal / return to chat list), otherwise the app is minimized (`moveTaskToBack`) and not closed.

Incoming call on a locked phone: Rust (`chat/voice.rs` -> `android.rs`, JNI) invokes
`CallService.showIncoming` - a notification channel called "Incoming calls" (`IMPORTANCE_HIGH`, ringtone,
vibration) with `fullScreenIntent` -> `IncomingCallActivity` above the lock screen ("Answer"/"Decline");
the same buttons are present in the notification itself. The action reaches MainActivity (`ntfry_call`) -> JS
`ntfry-call-action` -> `voice_answer`. `hideIncoming` is called on answer/decline/timeout. Android 14+:
for Play apps, the user must enable `USE_FULL_SCREEN_INTENT` permission - on first launch we open the proper settings screen (sideload builds are granted immediately).

Telecom (`android/NtfryConnectionService.kt`, self-managed `ConnectionService`, API 26+): every call is registered with the system Telecom - the OS routes audio itself (Bluetooth SCO / wired headset / speakerphone / earpiece), and headset buttons "answer/end" arrive as `onAnswer`/`onDisconnect` -> JS (`ntfry-call-action`: answer/decline/hangup/mute/unmute). A cellular call correctly places ours on hold (microphone muted). State is emitted from Rust
(`voice.rs emit_state` -> `android.rs` -> `CallService.callState`): calling -> `placeCall`, active ->
`setActive`, ended -> `setDisconnected(reason)`. Incoming calls use `addNewIncomingCall` -> `onShowIncomingCallUi`
-> our full-screen notification; if Telecom refuses (`isIncomingCallPermitted=false`) -
notification is shown directly. In the Android call bar there is a "Speaker" button (`voice_audio_route`).
A right swipe across the chat list (≥90px, quick, horizontal) -> chat list.

APK/AAB signing: the Tauri template itself does not read `keystore.properties` (which is why
`app-universal-release-unsigned.apk` appears). `yarn android:setup` adds `signingConfigs.release` to `app/build.gradle.kts`
from `src-tauri/gen/android/keystore.properties`:
```
storePassword=...
keyPassword=...
keyAlias=ntfrychat
storeFile=../keys/ntfrychat.jks     # path to the KEYSTORE FILE, not a directory; use forward slashes
```
Create the key: `keytool -genkey -v -keystore ntfrychat.jks -keyalg RSA -keysize 2048 -validity 10000 -alias ntfrychat`.
Without a keystore.properties file, release builds are created as before (unsigned).
`android:build`/`android:dev` automatically run `android-setup.cjs` (patches Gradle + checks
`keystore.properties`: empty passwords, storeFile is a directory / missing -> warning "SIGNING WILL NOT BE APPLIED"). Verify the signature of the finished APK:
```
"%ANDROID_HOME%\build-tools\34.0.0\apksigner.bat" verify --print-certs app-arm64-release.apk
keytool -printcert -jarfile app-arm64-release.apk
```
The signed file is named `app-<abi>-release.apk` (without `-unsigned`).

APK versioning: the single source of truth is `package.json` -> `scripts/sync-version.cjs` rewrites
`tauri.conf.json` (`version`, `bundle.android.versionCode = major·1e6 + minor·1e3 + patch`) and
`Cargo.toml`; it runs automatically before `android:*` and `build:*`, manually via `yarn version:sync`.
To bump the release: `yarn version patch` (or edit version in package.json) -> `yarn android:build`.

Calls: before the first call, the UI invokes `getUserMedia({audio})` - the WebView (wry) requests the system
`RECORD_AUDIO` permission, and `on_permission_request(Microphone->Allow)` in `lib.rs` confirms it, after which
cpal/oboe is used. Notifications for messages/calls come from `tauri-plugin-notification`. Mobile layout
(≤760px): chat list and conversation are separate screens; a "‹" button appears in the conversation header.
