// Регресс-проверка локалей (v1.135.206): каждая строка src/i18n/{ru,en,id}.ts компилируется
// тем же компилятором, что использует vue-i18n в рантайме. В dev ошибка компиляции — лишь
// предупреждение в консоли, а в production `t()` бросает SyntaxError и компонент исчезает
// (кейс: неэкранированный `@имя` во вкладке DAPPS профиля, 1.135.205). Поэтому ловим на сборке.
//
// Ошибка сборки выглядит так:
//   [i18n-check] ru.ts → profile.resyncHint: Invalid linked format
//     "Перечитать из блокчейна имена, своё @имя и подписчиков…"
//     подсказка: символ @ экранируйте как {'@'} (строку берите в двойные кавычки)
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { build as esbuild } from 'esbuild'
import { baseCompile, errorMessages } from '@intlify/message-compiler'

const LOCALES = ['ru', 'en', 'id']

/** Собрать локаль (TS → ESM в памяти) и импортировать её default-экспорт. */
async function loadLocale(i18nDir, name) {
  const entry = path.join(i18nDir, `${name}.ts`)
  const out = await esbuild({ entryPoints: [entry], bundle: true, write: false, format: 'esm', platform: 'neutral', target: 'es2020', logLevel: 'silent' })
  const code = out.outputFiles[0].text
  const mod = await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`)
  return mod.default
}

/** Обойти дерево сообщений: (keyPath, value) для каждой строки. */
function* walk(obj, prefix = '') {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k
    if (typeof v === 'string') yield [key, v]
    else if (v && typeof v === 'object') yield* walk(v, key)
  }
}

function compileErrors(msg) {
  const errs = []
  try {
    baseCompile(msg, { onError: (e) => errs.push(e) })
  } catch (e) {
    errs.push(e)
  }
  return errs
}

function describe(e) {
  const code = e && typeof e.code === 'number' ? e.code : undefined
  return (code !== undefined && errorMessages[code]) || e.message || String(e)
}

export async function checkLocales(i18nDir) {
  const problems = []
  for (const name of LOCALES) {
    let messages
    try {
      messages = await loadLocale(i18nDir, name)
    } catch (e) {
      problems.push(`${name}.ts: не удалось загрузить локаль — ${e.message || e}`)
      continue
    }
    for (const [key, msg] of walk(messages)) {
      const errs = compileErrors(msg)
      if (!errs.length) continue
      const why = [...new Set(errs.map(describe))].join('; ')
      const hint = /@/.test(msg) && /linked/i.test(why) ? `\n    подсказка: символ @ экранируйте как {'@'} (строку берите в двойные кавычки)` : ''
      const short = msg.length > 140 ? msg.slice(0, 137) + '…' : msg
      problems.push(`${name}.ts → ${key}: ${why}\n    ${JSON.stringify(short)}${hint}`)
    }
  }
  return problems
}

/**
 * Vite-плагин: при `vite build` (и в dev при старте) проверяет все локали; при ошибках
 * сборка падает. `strict: false` — только предупреждать (для отладки).
 */
export default function i18nCheck({ dir, strict = true } = {}) {
  const i18nDir = dir
  let isBuild = false
  return {
    name: 'netfory-i18n-check',
    configResolved(cfg) {
      isBuild = cfg.command === 'build'
    },
    async buildStart() {
      const problems = await checkLocales(i18nDir)
      if (!problems.length) {
        const okMsg = `[i18n-check] локали ${LOCALES.join('/')} — все сообщения компилируются`
        if (typeof this.info === 'function') this.info(okMsg)
        else console.log(okMsg)
        return
      }
      const text = `[i18n-check] найдено ${problems.length} ошибок в строках локализации:\n\n  ` + problems.join('\n\n  ') + '\n'
      if (strict && isBuild) this.error(text)
      else this.warn(text)
    },
  }
}

// Прямой запуск: `node scripts/vite-plugin-i18n-check.js` (выход 1 при ошибках) — для CI/pre-commit.
if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  const dir = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../src/i18n')
  const problems = await checkLocales(dir)
  if (problems.length) {
    console.error(`[i18n-check] ${problems.length} ошибок:\n\n  ` + problems.join('\n\n  '))
    process.exit(1)
  }
  console.log(`[i18n-check] ok (${LOCALES.join('/')})`)
}
