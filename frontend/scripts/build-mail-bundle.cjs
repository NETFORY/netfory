// Универсальный билд SmartNet + встроенный Oxid Mail.
//
// Что делает (по шагам):
//   1) Собирает Oxid Mail и копирует бинарь в /frontend/src-tauri/
//      (`oxid-mail.exe` на Windows, `oxid-mail` на Linux/macOS).
//   2) Явно запускает `yarn build` (vite) ИЗ КАТАЛОГА /frontend/ — так vite
//      всегда получает правильный cwd и пишет `dist/` рядом со своим
//      конфигом. Это устраняет ошибку `Unable to find your web assets`,
//      когда `src-tauri/` лежит не внутри frontend, и Tauri исполнял
//      `beforeBuildCommand` из другого cwd.
//   3) Генерирует временный merge-конфиг `src-tauri/tauri.mail.gen.conf.json`
//      с `build.frontendDist = "dist"` и кладёт СВЕЖУЮ КОПИЮ dist в
//      `src-tauri/dist`. Почему так: абсолютный путь на Windows ("D:/...")
//      Tauri парсит как URL со схемой `d:` → приложение грузило файловый
//      индекс папки; а базу резолва ОТНОСИТЕЛЬНОГО пути разные версии CLI
//      выбирают по-разному (cwd на Ubuntu-логе v1.135.6, src-tauri/ на
//      Windows v1.135.12). Копия в обеих кандидатных базах (cwd=/frontend
//      и /frontend/src-tauri) делает "dist" валидным при любой семантике.
//   4) Запускает `tauri build` с платформенно-специфичным merge-конфигом
//      (resources: `oxid-mail.exe` на Windows, `oxid-mail` на Linux/macOS)
//      и с набором bundle-форматов, поддерживаемых текущей платформой:
//        Windows → msi
//        Linux   → deb + appimage
//        macOS   → dmg + app
//      Все merge-конфиги переопределяют `build.beforeBuildCommand` пустой
//      строкой, чтобы Tauri не запускал vite повторно (мы уже собрали).
//
// Использование:
//   yarn tauri:build:bonsai:mail
//
// Опциональные переменные окружения:
//   TAURI_BUNDLES     — переопределяет список bundle-форматов
//   TAURI_EXTRA_ARGS  — дополнительные CLI-аргументы для `tauri build`
//   SKIP_FRONTEND     — пропустить шаг vite-билда (если dist уже собран)

const { spawnSync } = require('child_process')
const path = require('path')
const fs = require('fs')

const frontend = path.resolve(__dirname, '..')

function run(cmd, args, opts = {}) {
  console.log(`[build-mail-bundle] (cwd=${opts.cwd || frontend}) ${cmd} ${args.join(' ')}`)
  const r = spawnSync(cmd, args, { cwd: opts.cwd || frontend, stdio: 'inherit', shell: true })
  if (r.status !== 0) process.exit(r.status ?? 1)
}

// 1) Собираем Oxid Mail и копируем бинарь.
run('node', ['scripts/build-oxid-mail.cjs'])

// 2) Явно запускаем vite build из /frontend/ — cwd жёстко привязан к папке,
//    где лежит package.json + vite.config.js. Это гарантирует, что `dist/`
//    окажется в предсказуемом месте (см. vite.config.js: outDir абсолютный).
if (!process.env.SKIP_FRONTEND) {
  run('yarn', ['build'])
}
const distDir = path.join(frontend, 'dist')
if (!fs.existsSync(path.join(distDir, 'index.html'))) {
  console.error(`[build-mail-bundle] FATAL: ${distDir}/index.html не найден после vite build.`)
  console.error('  Проверьте vite.config.js (root, build.outDir) и что frontend содержит index.html.')
  process.exit(1)
}
console.log(`[build-mail-bundle] frontend dist готов: ${distDir}`)

// 3) Определяем платформенно-специфичные параметры.
const platform = process.platform // 'win32' | 'darwin' | 'linux' | ...
const isWin = platform === 'win32'
const isMac = platform === 'darwin'

const baseConfig = isWin
  ? 'tauri.mail.conf.json'
  : isMac
    ? 'tauri.mail.macos.conf.json'
    : 'tauri.mail.linux.conf.json'

// 4) Генерируем merge-конфиг с frontendDist = "dist" (плоский относительный
//    путь никогда не парсится как URL). База резолва у разных версий CLI
//    разная (cwd ИЛИ src-tauri/), поэтому дублируем свежий dist в
//    src-tauri/dist — путь корректен из обеих баз. Копия в .gitignore (dist).
const srcTauri = path.join(frontend, 'src-tauri')
const srcTauriDist = path.join(srcTauri, 'dist')
fs.rmSync(srcTauriDist, { recursive: true, force: true })
fs.cpSync(distDir, srcTauriDist, { recursive: true })
console.log(`[build-mail-bundle] копия dist для src-tauri-базы: ${srcTauriDist}`)
const merged = JSON.parse(fs.readFileSync(path.join(srcTauri, baseConfig), 'utf8'))
merged.build = merged.build || {}
merged.build.frontendDist = 'dist'
const genConfig = path.join(srcTauri, 'tauri.mail.gen.conf.json')
fs.writeFileSync(genConfig, JSON.stringify(merged, null, 2) + '\n')
console.log(`[build-mail-bundle] merge-конфиг: ${genConfig} (frontendDist=${merged.build.frontendDist})`)

const bundles = process.env.TAURI_BUNDLES || (isWin ? 'msi' : isMac ? 'dmg,app' : 'deb,appimage')

const extra = (process.env.TAURI_EXTRA_ARGS || '').trim().split(/\s+/).filter(Boolean)

// 5) Запускаем `tauri build`. Merge-конфиг обнуляет `beforeBuildCommand`,
//    поэтому vite повторно не запускается.
run('npx', ['tauri', 'build', '--features', 'p2p,bonsai', '--bundles', bundles, '--config', path.join('src-tauri', 'tauri.mail.gen.conf.json'), ...extra])
