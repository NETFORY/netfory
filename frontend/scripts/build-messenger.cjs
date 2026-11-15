// Сборка NETFORY Messenger (ntfry-messenger) и копирование бинаря в src-tauri
// для упаковки в единый инсталлятор SmartNet (как oxid-mail).
// Использование: node scripts/build-messenger.cjs  (вызывается из tauri:build:bonsai:full:*)
const { execSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const frontend = path.resolve(__dirname, '..')
const root = path.resolve(frontend, '..', 'messenger')

function run(cmd, cwd) {
  console.log(`\n[build-messenger] ${cmd}  (cwd: ${cwd})`)
  // CMake ≥ 4 отвергает старый cmake_minimum_required у libopus (audiopus_sys) — разрешаем политику
  execSync(cmd, { cwd, stdio: 'inherit', shell: true, env: { ...process.env, CMAKE_POLICY_VERSION_MINIMUM: process.env.CMAKE_POLICY_VERSION_MINIMUM || '3.5' } })
}

run('yarn install', root)
// только бинарь (без собственного инсталлятора мессенджера): vite build + cargo release
run('yarn tauri build --no-bundle', root)

const exe = process.platform === 'win32' ? 'ntfry-messenger.exe' : 'ntfry-messenger'
const candidates = [
  process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'release', exe),
  path.join(root, 'src-tauri', 'target', 'release', exe),
  path.join(root, 'target', 'release', exe),
].filter(Boolean)

const src = candidates.find((p) => fs.existsSync(p))
if (!src) {
  console.error('[build-messenger] Бинарь не найден. Искал:\n' + candidates.join('\n'))
  process.exit(1)
}
const dest = path.join(frontend, 'src-tauri', exe)
fs.copyFileSync(src, dest)
console.log(`[build-messenger] OK: ${src} -> ${dest}`)

// Linux/macOS: sidecar через bundle.externalBin требует суффикс host target triple.
if (process.platform !== 'win32') {
  let triple = ''
  try {
    triple = execSync('rustc -vV').toString().match(/host:\s*(\S+)/)[1]
  } catch (e) {
    console.error('[build-messenger] FATAL: не удалось определить host triple через `rustc -vV`:', e.message)
    process.exit(1)
  }
  const sidecar = path.join(frontend, 'src-tauri', `ntfry-messenger-${triple}`)
  fs.copyFileSync(src, sidecar)
  fs.chmodSync(sidecar, 0o755)
  fs.chmodSync(dest, 0o755)
  console.log(`[build-messenger] sidecar: ${src} -> ${sidecar}`)
}
