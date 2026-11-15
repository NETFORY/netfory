#!/usr/bin/env node
// Обёртка над `tauri android <cmd>`: готовит окружение для кросс-сборки нативных зависимостей
// (libopus через audiopus_sys/cmake, oboe для cpal) на Windows/macOS/Linux и запускает CLI.
//   • cmake + ninja: PATH → иначе <ANDROID_HOME>/cmake/<ver>/bin (ставится вместе с SDK);
//   • CMAKE_GENERATOR=Ninja (на Windows cmake по умолчанию выбрал бы Visual Studio — для
//     Android-таргета это ошибка);
//   • ANDROID_NDK_ROOT/ANDROID_NDK_HOME из NDK_HOME или <ANDROID_HOME>/ndk/<ver> — CMake в режиме
//     CMAKE_SYSTEM_NAME=Android находит по ним clang из NDK;
//   • CMAKE_POLICY_VERSION_MINIMUM=3.5 — libopus с CMake 4.x;
//   • после `init` автоматически запускает scripts/android-setup.cjs.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const isWin = process.platform === 'win32'
const exe = isWin ? '.exe' : ''
const args = process.argv.slice(2)
const env = { ...process.env }

function newestSubdir(dir) {
  if (!dir || !fs.existsSync(dir)) return null
  const subs = fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => e.name)
    .sort((a, b) => b.localeCompare(a, undefined, { numeric: true }))
  return subs.length ? path.join(dir, subs[0]) : null
}
function onPath(bin) {
  const r = spawnSync(isWin ? 'where' : 'which', [bin], { stdio: 'ignore' })
  return r.status === 0
}

const sdk = env.ANDROID_HOME || env.ANDROID_SDK_ROOT
if (!sdk) {
  console.error('ANDROID_HOME не задан (Android Studio → SDK Manager → путь к SDK)')
  process.exit(1)
}
env.ANDROID_HOME = sdk
env.ANDROID_SDK_ROOT = sdk

// NDK
let ndk = env.NDK_HOME || env.ANDROID_NDK_HOME || env.ANDROID_NDK_ROOT || newestSubdir(path.join(sdk, 'ndk'))
if (!ndk || !fs.existsSync(ndk)) {
  console.error('NDK не найден: задайте NDK_HOME или установите NDK (SDK Manager → SDK Tools → NDK (Side by side))')
  process.exit(1)
}
env.NDK_HOME = ndk
env.ANDROID_NDK_HOME = ndk
env.ANDROID_NDK_ROOT = ndk
env.ANDROID_NDK = ndk

// cmake + ninja
// Windows: ключ окружения называется `Path` — у копии process.env регистр сохраняется, второй ключ
// `PATH` затёр бы его в дочернем процессе (симптом: «node не является … командой»).
const pathKey = Object.keys(env).find((k) => k.toUpperCase() === 'PATH') || 'PATH'
if (!onPath('cmake')) {
  const cmakeDir = newestSubdir(path.join(sdk, 'cmake'))
  const bin = cmakeDir ? path.join(cmakeDir, 'bin') : null
  if (bin && fs.existsSync(path.join(bin, `cmake${exe}`))) {
    env[pathKey] = `${bin}${path.delimiter}${env[pathKey] || ''}`
    env.CMAKE = path.join(bin, `cmake${exe}`)
    if (fs.existsSync(path.join(bin, `ninja${exe}`))) env.CMAKE_MAKE_PROGRAM = path.join(bin, `ninja${exe}`)
    console.log(`cmake: ${bin}`)
  } else {
    console.error('cmake не найден ни в PATH, ни в <ANDROID_HOME>/cmake — SDK Manager → SDK Tools → CMake')
    process.exit(1)
  }
}
if (!env.CMAKE_GENERATOR) env.CMAKE_GENERATOR = 'Ninja'
if (!env.CMAKE_POLICY_VERSION_MINIMUM) env.CMAKE_POLICY_VERSION_MINIMUM = '3.5'

// libc++_shared.so из NDK → app/src/main/jniLibs/<abi>/ (oboe/cpal собран с shared-stdcxx; без
// библиотеки в APK — UnsatisfiedLinkError «cannot locate symbol __cxa_pure_virtual» на старте)
function bundleLibcxx() {
  const prebuilt = path.join(ndk, 'toolchains', 'llvm', 'prebuilt')
  const host = newestSubdir(prebuilt)
  if (!host) {
    console.warn('NDK: не найден toolchains/llvm/prebuilt — libc++_shared.so не скопирован')
    return
  }
  const abis = {
    'arm64-v8a': 'aarch64-linux-android',
    'armeabi-v7a': 'arm-linux-androideabi',
    'x86_64': 'x86_64-linux-android',
    'x86': 'i686-linux-android',
  }
  const jni = path.join(root, 'src-tauri', 'gen', 'android', 'app', 'src', 'main', 'jniLibs')
  for (const [abi, triple] of Object.entries(abis)) {
    const src = path.join(host, 'sysroot', 'usr', 'lib', triple, 'libc++_shared.so')
    if (!fs.existsSync(src)) continue
    const dstDir = path.join(jni, abi)
    fs.mkdirSync(dstDir, { recursive: true })
    const dst = path.join(dstDir, 'libc++_shared.so')
    if (!fs.existsSync(dst) || fs.statSync(dst).size !== fs.statSync(src).size) fs.copyFileSync(src, dst)
  }
  const have = Object.keys(abis).filter((abi) => fs.existsSync(path.join(jni, abi, 'libc++_shared.so')))
  if (have.length) console.log(`libc++_shared.so в jniLibs: ${have.join(', ')}`)
  else console.error(`libc++_shared.so НЕ НАЙДЕН в ${host}/sysroot/usr/lib/<triple>/ — APK упадёт на старте (UnsatisfiedLinkError)`)
}
if (args[0] !== 'init' && fs.existsSync(path.join(root, 'src-tauri', 'gen', 'android', 'app'))) bundleLibcxx()

// версия APK = package.json (versionName/versionCode через tauri.conf.json)
const v = spawnSync(process.execPath, [path.join(__dirname, 'sync-version.cjs')], { stdio: 'inherit', cwd: root })
if (v.status !== 0) process.exit(v.status ?? 1)

// build/dev: донастроить gen/android (подпись, манифест, Kotlin) — патчи могли обновиться с прошлого init
if (args[0] !== 'init' && fs.existsSync(path.join(root, 'src-tauri', 'gen', 'android', 'app'))) {
  const s = spawnSync(process.execPath, [path.join(__dirname, 'android-setup.cjs')], { stdio: 'inherit', cwd: root })
  if (s.status !== 0) process.exit(s.status ?? 1)
}

// dev по USB (по умолчанию): в dev-сборке Tauri проксирует UI с dev-сервера Vite на ПК, и на Windows
// CLI подставляет публичный IP машины — телефон часто до него не доходит (брандмауэр, VPN-адаптер,
// другая Wi-Fi) → через 30 с «custom protocol timed out» и пустой chrome-error. Вместо этого
// пробрасываем порты через adb reverse и говорим Tauri/Vite работать на 127.0.0.1.
// `--lan` — прежнее поведение (публичный IP / свой `--host`).
const tauriArgs = args.filter((a) => a !== '--lan')
if (args[0] === 'dev' && !args.includes('--lan') && !args.some((a) => a.startsWith('--host'))) {
  const adb = onPath('adb') ? 'adb' : path.join(sdk, 'platform-tools', `adb${exe}`)
  if (!fs.existsSync(adb) && adb !== 'adb') {
    console.warn('adb не найден (platform-tools) — USB-проброс портов пропущен, запустите с --lan')
  } else {
    const devices = spawnSync(adb, ['devices'], { encoding: 'utf8' })
    const online = (devices.stdout || '').split(/\r?\n/).slice(1).filter((l) => /\tdevice$/.test(l.trim()))
    if (!online.length) console.warn('adb: устройство не найдено/не авторизовано — проброс портов выполнится, когда Tauri подключится')
    for (const port of [3010, 3011]) {
      const rv = spawnSync(adb, ['reverse', `tcp:${port}`, `tcp:${port}`], { encoding: 'utf8' })
      if (rv.status !== 0) console.warn(`adb reverse tcp:${port}: ${(rv.stderr || rv.stdout || '').trim()}`)
    }
    console.log('USB dev: adb reverse tcp:3010/3011 → dev-сервер для телефона = http://127.0.0.1:3010 (Wi-Fi не нужен)')
    tauriArgs.push('--host=127.0.0.1')
  }
}

const tauriBin = path.join(root, 'node_modules', '.bin', isWin ? 'tauri.cmd' : 'tauri')
console.log(`> tauri android ${tauriArgs.join(' ')}   (NDK: ${ndk})`)
const r = spawnSync(tauriBin, ['android', ...tauriArgs], { stdio: 'inherit', env, cwd: root, shell: isWin })
if (r.status !== 0) process.exit(r.status ?? 1)

// после build: показать РЕАЛЬНО свежие артефакты (tauri печатает и старые файлы из outputs/) и статус подписи
if (args[0] === 'build') {
  const out = path.join(root, 'src-tauri', 'gen', 'android', 'app', 'build', 'outputs')
  const found = []
  ;(function walk(d) {
    if (!fs.existsSync(d)) return
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name)
      if (e.isDirectory()) walk(p)
      else if (/\.(apk|aab)$/.test(e.name)) found.push({ p, t: fs.statSync(p).mtimeMs })
    }
  })(out)
  found.sort((a, b) => b.t - a.t)
  console.log('\nАртефакты (новые сверху):')
  for (const f of found.slice(0, 8)) {
    const age = Math.round((Date.now() - f.t) / 60000)
    const stale = Date.now() - f.t > 15 * 60000 ? '  (СТАРЫЙ — не из этой сборки)' : ''
    const unsigned = /-unsigned\./.test(f.p) ? '  [UNSIGNED]' : '  [signed]'
    console.log(`  ${path.relative(root, f.p)}  ${age} мин назад${unsigned}${stale}`)
  }
  if (found.length && Date.now() - found[0].t > 15 * 60000) console.warn('Свежих APK/AAB нет — сборка Gradle не дошла до упаковки, смотрите ошибки выше.')
}

if (args[0] === 'init') {
  const s = spawnSync(process.execPath, [path.join(__dirname, 'android-setup.cjs')], { stdio: 'inherit', cwd: root })
  process.exit(s.status ?? 1)
}
