// Сборка TUN-помощника системного VPN Mycelium (mycelium-tun, крейт /mycelium, фича `tun`)
// и копирование бинаря в src-tauri для упаковки рядом с Netfory (как oxid-mail / ntfry-messenger).
// Использование: node scripts/build-mycelium-tun.cjs  (вызывается из tauri:build:bonsai:full:*)
// Windows: рядом с mycelium-tun.exe нужен wintun.dll — скрипт скачивает его с wintun.net в src-tauri/.
const { execSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const frontend = path.resolve(__dirname, '..')
const root = path.resolve(frontend, '..', 'mycelium')

function run(cmd, cwd) {
  console.log(`\n[build-mycelium-tun] ${cmd}  (cwd: ${cwd})`)
  execSync(cmd, { cwd, stdio: 'inherit', shell: true })
}

run('cargo build --release --features tun --bin mycelium-tun', root)

const exe = process.platform === 'win32' ? 'mycelium-tun.exe' : 'mycelium-tun'
const candidates = [
  process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'release', exe),
  path.join(root, 'target', 'release', exe),
].filter(Boolean)
const src = candidates.find((p) => fs.existsSync(p))
if (!src) {
  console.error('[build-mycelium-tun] Бинарь не найден. Искал:\n' + candidates.join('\n'))
  process.exit(1)
}
const dest = path.join(frontend, 'src-tauri', exe)
fs.copyFileSync(src, dest)
console.log(`[build-mycelium-tun] OK: ${src} -> ${dest}`)

if (process.platform === 'win32') {
  // wintun.dll обязателен для TUN на Windows; tauri.full.conf.json ждёт его в ресурсах — скачиваем, если нет
  const dll = path.join(frontend, 'src-tauri', 'wintun.dll')
  if (!fs.existsSync(dll)) {
    const zip = path.join(frontend, 'src-tauri', 'wintun.zip')
    const arch = process.arch === 'arm64' ? 'arm64' : 'amd64'
    try {
      run(`powershell -NoProfile -Command "Invoke-WebRequest -Uri https://www.wintun.net/builds/wintun-0.14.1.zip -OutFile '${zip}'; Expand-Archive -Force '${zip}' '${zip}.d'; Copy-Item '${zip}.d/wintun/bin/${arch}/wintun.dll' '${dll}'; Remove-Item -Recurse -Force '${zip}','${zip}.d'"`, frontend)
      console.log(`[build-mycelium-tun] wintun.dll (${arch}) -> ${dll}`)
    } catch (e) {
      console.error('[build-mycelium-tun] FATAL: не удалось получить wintun.dll — скачайте https://www.wintun.net и положите bin/' + arch + '/wintun.dll в src-tauri/wintun.dll')
      process.exit(1)
    }
  }
} else {
  let triple = ''
  try {
    triple = execSync('rustc -vV').toString().match(/host:\s*(\S+)/)[1]
  } catch (e) {
    console.error('[build-mycelium-tun] FATAL: не удалось определить host triple через `rustc -vV`:', e.message)
    process.exit(1)
  }
  const sidecar = path.join(frontend, 'src-tauri', `mycelium-tun-${triple}`)
  fs.copyFileSync(src, sidecar)
  fs.chmodSync(sidecar, 0o755)
  fs.chmodSync(dest, 0o755)
  console.log(`[build-mycelium-tun] sidecar: ${src} -> ${sidecar}`)
}
