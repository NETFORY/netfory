// SmartNet // lib/fileTags.ts
//
// Парсинг и построение JSON-тегов файлов из файлового хранилища.
// Формат `FileManifest.tag` — opaque JSON-строка, но фронт обязан её
// нормализовать в структурированный `ParsedTag`, чтобы можно было
// строить фильтры (`All / Dev / Music / Image / Other / Private`) и
// иерархию `Repo → Version` внутри Dev-вкладки без парсинга строки по
// месту в каждом компоненте.
//
// Схема:
//   {"cat":"dev",   "repo":"git-remote-dev", "ver":"1.55.47", "os":"win"}
//   {"cat":"music"}
//   {"cat":"image", "kind":"avatar" | "wall" | "cover"}
//   {"cat":"other"}
//   пустая строка / невалидный JSON → { cat: "other" }
//
// Приватные (E2EE) файлы определяются НЕ по тегу, а по `encrypted:true`
// в самом манифесте — так у нас единый источник правды.

/** OS-тег бинарника релиза (детект по расширению файла). */
export type ReleaseOS = 'win' | 'macos' | 'linux' | 'android' | 'archive' | ''

/** Верхнеуровневая категория, которую видит UI в табах My Files. */
export type FileCategory = 'dev' | 'music' | 'image' | 'other' | 'private'

export interface ParsedTag {
  cat: FileCategory
  /** для `cat=dev` — имя репозитория (`git-remote-dev`). */
  repo?: string
  /** для `cat=dev` — версия/тег релиза (`1.55.47`). */
  ver?: string
  /** для `cat=dev` — целевая ОС (`win|macos|linux|android|archive|''`). */
  os?: ReleaseOS
  /** для `cat=image` — назначение (`avatar|wall|cover`). */
  kind?: string
}

/** Разобрать opaque tag-строку. Никогда не бросает — возвращает `other` при ошибке. */
export function parseFileTag(tag?: string): ParsedTag {
  const raw = (tag || '').trim()
  if (!raw) return { cat: 'other' }
  try {
    const obj = JSON.parse(raw)
    if (obj && typeof obj === 'object' && typeof obj.cat === 'string') {
      const cat = obj.cat as FileCategory
      if (cat === 'dev' || cat === 'music' || cat === 'image' || cat === 'other' || cat === 'private') {
        return {
          cat,
          repo: typeof obj.repo === 'string' ? obj.repo : undefined,
          ver: typeof obj.ver === 'string' ? obj.ver : undefined,
          os: typeof obj.os === 'string' ? (obj.os as ReleaseOS) : undefined,
          kind: typeof obj.kind === 'string' ? obj.kind : undefined,
        }
      }
    }
  } catch {
    /* fallthrough → other */
  }
  return { cat: 'other' }
}

/** Собрать JSON-строку тега (компактно, без пробелов). */
export function buildFileTag(tag: ParsedTag): string {
  const out: Record<string, string> = { cat: tag.cat }
  if (tag.repo) out.repo = tag.repo
  if (tag.ver) out.ver = tag.ver
  if (tag.os) out.os = tag.os
  if (tag.kind) out.kind = tag.kind
  return JSON.stringify(out)
}

/** Детект целевой ОС по расширению файла (для тегирования dev-релизов). */
export function osFromFilename(name: string): ReleaseOS {
  const n = name.toLowerCase()
  const ext = n.slice(n.lastIndexOf('.') + 1)
  if (ext === 'exe' || ext === 'msi') return 'win'
  if (ext === 'dmg' || ext === 'pkg') return 'macos'
  if (ext === 'deb' || ext === 'rpm' || ext === 'appimage') return 'linux'
  if (ext === 'apk' || ext === 'aab') return 'android'
  if (
    ext === 'zip' || ext === 'gz' || ext === 'tar' || ext === 'tgz' ||
    ext === 'xz' || ext === '7z' || ext === 'bz2'
  ) return 'archive'
  return ''
}

/** UI-метка ОС для чипов в таблице релизов. */
export function osLabel(os: ReleaseOS): string {
  switch (os) {
    case 'win':     return 'WIN'
    case 'macos':   return 'MACOS'
    case 'linux':   return 'LINUX'
    case 'android': return 'ANDROID'
    case 'archive': return 'ARCHIVE'
    default:        return ''
  }
}
