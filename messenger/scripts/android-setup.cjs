#!/usr/bin/env node
// Донастройка сгенерированного Android-проекта (после `tauri android init`). Кроссплатформенно
// (Windows без bash). Идемпотентно — повторный запуск ничего не дублирует.
//   1) AndroidManifest: разрешения (камера, микрофон, уведомления, foreground service, батарея)
//      + <service .CallService> (foreground service) + <activity .IncomingCallActivity> (экран звонка)
//      + <service .NtfryConnectionService> (Telecom: BT-гарнитуры, маршрут звука);
//   2) Kotlin: CallService.kt, IncomingCallActivity.kt, MainActivity.kt из messenger/android/ (пакет подставляется).
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const gen = path.join(root, 'src-tauri', 'gen', 'android')
const manifestPath = path.join(gen, 'app', 'src', 'main', 'AndroidManifest.xml')
if (!fs.existsSync(manifestPath)) {
  console.error(`нет ${manifestPath} — сначала: yarn android:init`)
  process.exit(1)
}

// ---- 1. манифест --------------------------------------------------------------------------
const PERMS = [
  'CAMERA',
  'RECORD_AUDIO',
  'MODIFY_AUDIO_SETTINGS',
  'POST_NOTIFICATIONS',
  'WAKE_LOCK',
  'FOREGROUND_SERVICE',
  'FOREGROUND_SERVICE_PHONE_CALL',
  'FOREGROUND_SERVICE_MICROPHONE',
  'MANAGE_OWN_CALLS',
  'REQUEST_IGNORE_BATTERY_OPTIMIZATIONS',
  'USE_FULL_SCREEN_INTENT',
  'VIBRATE',
]
const FEATURES = ['camera', 'microphone']
let m = fs.readFileSync(manifestPath, 'utf8')
const before = m
function insertBeforeApplication(line) {
  m = m.replace(/(\s*)<application/, `$1${line}$1<application`)
}
for (const p of PERMS) {
  if (!m.includes(`android.permission.${p}`)) insertBeforeApplication(`<uses-permission android:name="android.permission.${p}" />`)
}
for (const f of FEATURES) {
  if (!m.includes(`android.hardware.${f}`)) insertBeforeApplication(`<uses-feature android:name="android.hardware.${f}" android:required="false" />`)
}
// клавиатура сжимает WebView (строка ввода остаётся видимой), а не наезжает на него
const mainTag = m.match(/<activity[^>]*android:name="\.MainActivity"[^>]*>/)?.[0] || ''
if (mainTag && !mainTag.includes('windowSoftInputMode')) {
  m = m.replace(mainTag, mainTag.replace('android:name=".MainActivity"', 'android:name=".MainActivity"\n            android:windowSoftInputMode="adjustResize"'))
}
if (!m.includes('IncomingCallActivity')) {
  m = m.replace(/([ \t]*)<\/application>/, (_, ind) =>
    `${ind}    <activity\n${ind}        android:name=".IncomingCallActivity"\n${ind}        android:exported="false"\n${ind}        android:excludeFromRecents="true"\n${ind}        android:launchMode="singleInstance"\n${ind}        android:showWhenLocked="true"\n${ind}        android:turnScreenOn="true"\n${ind}        android:theme="@android:style/Theme.Material.NoActionBar" />\n${ind}</application>`,
  )
}
if (!m.includes('NtfryConnectionService')) {
  m = m.replace(/([ \t]*)<\/application>/, (_, ind) =>
    `${ind}    <service\n${ind}        android:name=".NtfryConnectionService"\n${ind}        android:exported="true"\n${ind}        android:permission="android.permission.BIND_TELECOM_CONNECTION_SERVICE">\n${ind}        <intent-filter>\n${ind}            <action android:name="android.telecom.ConnectionService" />\n${ind}        </intent-filter>\n${ind}    </service>\n${ind}</application>`,
  )
}
if (!m.includes('CallService')) {
  m = m.replace(/([ \t]*)<\/application>/, (_, ind) =>
    `${ind}    <service\n${ind}        android:name=".CallService"\n${ind}        android:exported="false"\n${ind}        android:foregroundServiceType="phoneCall|microphone" />\n${ind}</application>`,
  )
}
if (m !== before) {
  fs.writeFileSync(manifestPath, m)
  console.log('AndroidManifest.xml обновлён')
} else {
  console.log('AndroidManifest.xml уже настроен')
}

// ---- 1b. подпись release: app/build.gradle.kts читает gen/android/keystore.properties -----------
// Шаблон tauri сам keystore.properties НЕ читает (поэтому выходит app-universal-release-unsigned.apk).
// Добавляем signingConfigs как в документации Tauri; без файла keystore.properties сборка идёт как раньше.
const gradlePath = path.join(gen, 'app', 'build.gradle.kts')
if (fs.existsSync(gradlePath)) {
  let g = fs.readFileSync(gradlePath, 'utf8')
  const g0 = g
  if (!g.includes('import java.util.Properties')) g = 'import java.util.Properties\n' + g
  // ранний вариант патча использовал FileInputStream без импорта (шаблон tauri импортирует только Properties)
  if (g.includes('FileInputStream(') && !g.includes('import java.io.FileInputStream')) g = 'import java.io.FileInputStream\n' + g
  // Windows: файл может быть с CRLF — регулярки терпимы к \r
  const eol = g.includes('\r\n') ? '\r\n' : '\n'
  if (!g.includes('signingConfigs {')) {
    g = g.replace(/\r?\nandroid \{\r?\n/, `
android {
    signingConfigs {
        create("release") {
            val keystorePropertiesFile = rootProject.file("keystore.properties")
            if (keystorePropertiesFile.exists()) {
                val keystoreProperties = Properties()
                keystorePropertiesFile.inputStream().use { keystoreProperties.load(it) }
                keyAlias = keystoreProperties["keyAlias"] as String
                keyPassword = keystoreProperties["keyPassword"] as String
                storeFile = file(keystoreProperties["storeFile"] as String)
                storePassword = keystoreProperties["storePassword"] as String
                println("ntfry: release signing = " + keyAlias + " @ " + storeFile)
            }
        }
    }
`)
  }
  if (!g.includes('signingConfigs.getByName("release")')) {
    g = g.replace(/getByName\("release"\)\s*\{\r?\n/, (m) =>
      `${m}            if (rootProject.file("keystore.properties").exists()) signingConfig = signingConfigs.getByName("release")${eol}`,
    )
  }
  if (eol === '\r\n') g = g.replace(/\r?\n/g, '\r\n')
  if (g !== g0) {
    fs.writeFileSync(gradlePath, g)
    console.log('app/build.gradle.kts: добавлена подпись release из keystore.properties')
  }
  // контроль: оба фрагмента обязаны присутствовать, иначе APK будет unsigned
  const okCfg = g.includes('signingConfigs {')
  const okUse = g.includes('signingConfigs.getByName("release")')
  if (okCfg && okUse) {
    console.log('app/build.gradle.kts: signingConfigs ✓, buildTypes.release.signingConfig ✓')
  } else {
    console.error(`app/build.gradle.kts: НЕ УДАЛОСЬ пропатчить (signingConfigs ${okCfg ? '✓' : '✗'}, release.signingConfig ${okUse ? '✓' : '✗'}).`)
    console.error('  Нестандартный шаблон? Пришлите файл ' + path.relative(root, gradlePath) + ' или вставьте блок из README «Подпись APK» вручную.')
  }
  const ks = path.join(gen, 'keystore.properties')
  if (!fs.existsSync(ks)) {
    fs.writeFileSync(
      ks,
      'storePassword=\nkeyPassword=\nkeyAlias=ntfrychat\n# полный путь к ФАЙЛУ keystore (.jks/.keystore), слэши прямые\nstoreFile=D:/path/to/ntfrychat.jks\n',
    )
    console.log('создан gen/android/keystore.properties — заполните пароли и storeFile')
  } else {
    // диагностика подписи: самые частые причины «…-unsigned.apk»
    const kv = {}
    for (const line of fs.readFileSync(ks, 'utf8').split(/\r?\n/)) {
      const m = line.match(/^\s*([A-Za-z]+)\s*=\s*(.*)$/)
      if (m) kv[m[1]] = m[2].trim()
    }
    const problems = []
    if (!kv.storePassword) problems.push('storePassword пустой')
    if (!kv.keyPassword) problems.push('keyPassword пустой')
    if (!kv.keyAlias) problems.push('keyAlias пустой')
    if (!kv.storeFile) problems.push('storeFile пустой')
    else if (!fs.existsSync(kv.storeFile)) problems.push(`storeFile не найден: ${kv.storeFile}`)
    else if (fs.statSync(kv.storeFile).isDirectory()) problems.push(`storeFile указывает на ПАПКУ, нужен файл .jks: ${kv.storeFile}`)
    if (problems.length) {
      console.warn('ПОДПИСЬ НЕ БУДЕТ ПРИМЕНЕНА (keystore.properties):\n  - ' + problems.join('\n  - '))
      console.warn('  → APK останется app-*-release-unsigned.apk')
    } else {
      console.log(`подпись: ${kv.keyAlias} @ ${kv.storeFile}`)
    }
  }
}

// ---- 2. Kotlin ----------------------------------------------------------------------------
function findMainActivity(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name)
    if (e.isDirectory()) {
      const r = findMainActivity(p)
      if (r) return r
    } else if (e.name === 'MainActivity.kt') return p
  }
  return null
}
const javaRoot = path.join(gen, 'app', 'src', 'main', 'java')
const mainActivity = findMainActivity(javaRoot)
if (!mainActivity) {
  console.error('MainActivity.kt не найден в gen/android — проект сгенерирован не полностью')
  process.exit(1)
}
const pkg = (fs.readFileSync(mainActivity, 'utf8').match(/^package\s+([\w.]+)/m) || [])[1]
if (!pkg) {
  console.error('не удалось определить package из MainActivity.kt')
  process.exit(1)
}
const pkgDir = path.dirname(mainActivity)
const tplDir = path.join(root, 'android')
for (const name of ['CallService.kt', 'IncomingCallActivity.kt', 'NtfryConnectionService.kt', 'MainActivity.kt']) {
  const src = fs.readFileSync(path.join(tplDir, name), 'utf8').replace(/__PACKAGE__/g, pkg)
  const dst = path.join(pkgDir, name)
  if (fs.existsSync(dst) && fs.readFileSync(dst, 'utf8') === src) {
    console.log(`${name} актуален`)
  } else {
    fs.writeFileSync(dst, src)
    console.log(`${name} записан (${pkg})`)
  }
}
console.log('готово')
