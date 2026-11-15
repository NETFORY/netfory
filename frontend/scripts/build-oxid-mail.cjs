// Сборка Oxid Mail и копирование бинаря в src-tauri для упаковки в MSI SmartNet.
// Использование: node scripts/build-oxid-mail.cjs  (вызывается из tauri:build:bonsai:mail)
const { execSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const frontend = path.resolve(__dirname, '..')
const oxidRoot = path.resolve(frontend, '..', 'oxid-mail')
const oxidUi = path.join(oxidRoot, 'ui')

function run(cmd, cwd) {
  console.log(`\n[build-oxid-mail] ${cmd}  (cwd: ${cwd})`)
  execSync(cmd, { cwd, stdio: 'inherit', shell: true })
}

run('yarn install', oxidUi)
run('yarn app:build', oxidUi) // = cd .. && tauri build (vite build + cargo release)

const exe = process.platform === 'win32' ? 'oxid-mail.exe' : 'oxid-mail'
// cargo workspace кладёт бинарь в target корня workspace (учитываем и CARGO_TARGET_DIR)
const candidates = [
  process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'release', exe),
  path.join(oxidRoot, 'target', 'release', exe),
  path.join(oxidRoot, 'src-tauri', 'target', 'release', exe),
].filter(Boolean)

const src = candidates.find((p) => fs.existsSync(p))
if (!src) {
  console.error('[build-oxid-mail] Бинарь не найден. Искал:\n' + candidates.join('\n'))
  process.exit(1)
}
const dest = path.join(frontend, 'src-tauri', exe)
fs.copyFileSync(src, dest)
console.log(`[build-oxid-mail] OK: ${src} -> ${dest}`)

// Linux/macOS: Tauri bundles исполняемые файлы через sidecar (`bundle.externalBin`),
// который требует имя с host target triple: oxid-mail-x86_64-unknown-linux-gnu.
// Sidecar кладётся рядом с основным бинарём (usr/bin в deb/AppImage,
// Contents/MacOS в .app) и сохраняет право на исполнение — в отличие от
// `resources`, которые уходят в usr/lib/<app>/ и не находились лаунчером.
if (process.platform !== 'win32') {
  let triple = ''
  try {
    triple = execSync('rustc -vV').toString().match(/host:\s*(\S+)/)[1]
  } catch (e) {
    console.error('[build-oxid-mail] FATAL: не удалось определить host triple через `rustc -vV`:', e.message)
    process.exit(1)
  }
  const sidecar = path.join(frontend, 'src-tauri', `oxid-mail-${triple}`)
  fs.copyFileSync(src, sidecar)
  fs.chmodSync(sidecar, 0o755)
  fs.chmodSync(dest, 0o755)
  console.log(`[build-oxid-mail] sidecar: ${src} -> ${sidecar}`)
}
