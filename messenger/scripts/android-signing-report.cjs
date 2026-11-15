#!/usr/bin/env node
// Что Gradle реально знает о подписи: `gradlew :app:signingReport` в gen/android.
// В выводе для Variant: universalRelease должны быть Config: release, Store: <путь .jks>, Alias: <alias>.
// «Config: null» = signingConfig к release не привязан → APK будет -unsigned.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const isWin = process.platform === 'win32'
const gen = path.join(root, 'src-tauri', 'gen', 'android')
const gradlew = path.join(gen, isWin ? 'gradlew.bat' : 'gradlew')
if (!fs.existsSync(gradlew)) {
  console.error(`нет ${gradlew} — сначала yarn android:init`)
  process.exit(1)
}
const r = spawnSync(gradlew, [':app:signingReport', '--console=plain', '-q'], { stdio: 'pipe', cwd: gen, shell: isWin, env: process.env })
const out = (r.stdout || '').toString() + (r.stderr || '').toString()
// показываем только release-варианты и строки ntfry
const lines = out.split(/\r?\n/)
let show = false
for (const l of lines) {
  if (/^Variant:/.test(l)) show = /[Rr]elease/.test(l)
  if (show || /ntfry:|FAILURE|error|Exception/i.test(l)) console.log(l)
}
if (!/Variant:.*[Rr]elease/.test(out)) console.log(out.slice(-3000))
process.exit(r.status ?? 1)
