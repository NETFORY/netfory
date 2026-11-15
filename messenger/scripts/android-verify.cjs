#!/usr/bin/env node
// Проверка подписи самого свежего APK/AAB из gen/android/app/build/outputs:
// apksigner (из <ANDROID_HOME>/build-tools/<ver>) → при его отсутствии keytool -printcert -jarfile.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const isWin = process.platform === 'win32'
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
const target = process.argv[2] || (found[0] && found[0].p)
if (!target || !fs.existsSync(target)) {
  console.error('APK не найден — соберите: yarn android:build')
  process.exit(1)
}
console.log(`файл: ${target}\nизменён: ${new Date(fs.statSync(target).mtimeMs).toLocaleString()}`)
if (/-unsigned\./.test(target)) console.warn('имя файла содержит -unsigned: Gradle собрал release без signingConfig')

const sdk = process.env.ANDROID_HOME || process.env.ANDROID_SDK_ROOT
let apksigner = null
if (sdk && fs.existsSync(path.join(sdk, 'build-tools'))) {
  const vers = fs.readdirSync(path.join(sdk, 'build-tools')).sort((a, b) => b.localeCompare(a, undefined, { numeric: true }))
  for (const v of vers) {
    const c = path.join(sdk, 'build-tools', v, isWin ? 'apksigner.bat' : 'apksigner')
    if (fs.existsSync(c)) { apksigner = c; break }
  }
}
if (apksigner && target.endsWith('.apk')) {
  console.log(`> apksigner verify --print-certs`)
  const r = spawnSync(apksigner, ['verify', '--verbose', '--print-certs', target], { stdio: 'inherit', shell: isWin })
  process.exit(r.status ?? 1)
}
console.log('> keytool -printcert -jarfile')
const r = spawnSync('keytool', ['-printcert', '-jarfile', target], { stdio: 'inherit', shell: isWin })
process.exit(r.status ?? 1)
