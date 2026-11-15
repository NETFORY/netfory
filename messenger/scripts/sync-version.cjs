#!/usr/bin/env node
// Единая версия мессенджера: package.json → src-tauri/tauri.conf.json и src-tauri/Cargo.toml.
// Android берёт versionName/versionCode из tauri.conf.json (tauri CLI пишет tauri.properties:
// versionCode = major*1_000_000 + minor*1_000 + patch, если bundle.android.versionCode не задан).
// Запускается автоматически перед `android:*` (scripts/android.cjs) и вручную: yarn version:sync
const fs = require('fs')
const path = require('path')

const root = path.resolve(__dirname, '..')
const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'))
const version = String(pkg.version || '').trim()
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  console.error(`package.json version «${version}» не в формате major.minor.patch`)
  process.exit(1)
}
const [maj, min, pat] = version.split('.').map(Number)
if (min > 999 || pat > 999) {
  console.error('minor/patch должны быть ≤ 999 (ограничение формулы versionCode Android)')
  process.exit(1)
}
const versionCode = maj * 1_000_000 + min * 1_000 + pat

const confPath = path.join(root, 'src-tauri', 'tauri.conf.json')
// правим текстом, чтобы не переформатировать весь файл
let conf = fs.readFileSync(confPath, 'utf8')
const conf0 = conf
let changed = []
conf = conf.replace(/^(\s*"version":\s*")[^"]*(")/m, (m, a, b) => `${a}${version}${b}`)
if (conf !== conf0) changed.push('tauri.conf.json version')
if (/"versionCode":\s*\d+/.test(conf)) {
  const c2 = conf.replace(/("versionCode":\s*)\d+/, `$1${versionCode}`)
  if (c2 !== conf) { conf = c2; changed.push('tauri.conf.json bundle.android.versionCode') }
} else {
  // вставляем "android": { "versionCode": N } первым полем в "bundle"
  const c2 = conf.replace(/("bundle":\s*\{\n)(\s*)/, (m, a, ind) => `${a}${ind}"android": { "versionCode": ${versionCode} },\n${ind}`)
  if (c2 !== conf) { conf = c2; changed.push('tauri.conf.json bundle.android.versionCode') }
  else console.warn('не нашёл секцию "bundle" в tauri.conf.json — versionCode не записан')
}
if (conf !== conf0) {
  JSON.parse(conf) // валидность
  fs.writeFileSync(confPath, conf)
}

const cargoPath = path.join(root, 'src-tauri', 'Cargo.toml')
const cargo = fs.readFileSync(cargoPath, 'utf8')
const cargo2 = cargo.replace(/^(\[package\][\s\S]*?^version = ")[^"]*(")/m, `$1${version}$2`)
if (cargo2 !== cargo) {
  fs.writeFileSync(cargoPath, cargo2)
  changed.push('Cargo.toml version')
}

console.log(`версия ${version} (versionCode ${versionCode})${changed.length ? ' → ' + changed.join(', ') : ' — всё синхронно'}`)
