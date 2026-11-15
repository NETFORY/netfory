/*
 * SmartNet // nav.config.ts
 *
 *   Единый типизированный источник правды для пунктов левого сайдбара.
 *   Раньше массив собирался прямо в `AppLayout.vue`, что делало добавление
 *   нового пункта (иконка + локаль + badge + фильтрация доступа) хрупким и
 *   размывало логику по компоненту в 1800 строк. Теперь достаточно правки
 *   этого файла — layout лишь рендерит результат.
 *
 *   Правила:
 *     - `id` совпадает с именем маршрута (`router.push({ name: id })`).
 *     - `icon` — символ юникода (не эмодзи), стилистически совпадающий с
 *       остальными пунктами. Для свёрнутого сайдбара обязателен непустой глиф.
 *     - `i18nKey` — ключ в `i18n/*.ts` (`nav.<key>`).
 *     - `badgeSource` — как посчитать бейдж-счётчик; лениво резолвится
 *       в лэйауте, чтобы этот файл оставался чистым от глобальных store'ов.
 *     - `gate` — необязательный предикат (доступ), например для скрытия
 *       глобального File Exchange для не-whitelisted пользователей.
 */

export type NavBadgeSource =
  | 'none'
  | 'apps.myApps'
  | 'discovery.pendingUpdates'
  | 'apps.installedApps'
  | 'devhub.openPrs'
  | 'seeders.sessionReplications'

export type NavGate = 'file-exchange' | 'developers'

/** UI-режим, в котором пункт виден. Пункт доступен, если mode ∈ modes. */
export type NavMode = 'user' | 'dev' | 'geek'

export interface NavItem {
  /** Route name (also used as an object key by AppLayout). */
  id: string
  /** Unicode glyph (never emoji) — must be non-empty for collapsed sidebar. */
  icon: string
  /** i18n key resolved as `nav.<i18nKey>`. Some items keep a static label. */
  i18nKey?: string
  /** Static (untranslated) label — takes precedence over `i18nKey` when set. */
  staticLabel?: string
  /** i18n-ключ подписи в USER-режиме (человеческий нейминг, напр. «Облако»). */
  userI18nKey?: string
  /** How to compute the badge counter (`0` = hide). */
  badgeSource?: NavBadgeSource
  /** Optional visibility gate resolved in AppLayout (e.g. env allow-list). */
  gate?: NavGate
  /**
   * Список UI-режимов, в которых пункт отображается. Пустой ⇒ виден везде.
   * USER-режим — «нормис» (только базовые страницы), DEV добавляет
   * пункты для авторов dApp'ов, GEEK показывает всё.
   */
  modes?: NavMode[]
}

export const NAV_ITEMS: NavItem[] = [
  { id: 'dashboard', icon: '▣', i18nKey: 'dashboard', modes: ['user', 'dev', 'geek'] },
  // Мои приложения — только для авторов dApp'ов (DEV) и «техно-режима» (GEEK).
  { id: 'apps', icon: '◆', i18nKey: 'apps', badgeSource: 'apps.myApps', modes: ['dev', 'geek'] },
  { id: 'discover', icon: '⊕', i18nKey: 'discover', badgeSource: 'discovery.pendingUpdates', modes: ['user', 'dev', 'geek'] },
  { id: 'installed', icon: '⊟', i18nKey: 'installed', badgeSource: 'apps.installedApps', modes: ['user', 'dev', 'geek'] },
  { id: 'files', icon: '⌹', i18nKey: 'files', gate: 'file-exchange', modes: ['user', 'dev', 'geek'] },
  // Storages группирует torrents / myfiles / music как «Хранилища & Медиа».
  // Сами страницы (torrents/myfiles/music) остаются доступны через прямые
  // sn://-ссылки и карточки внутри Storages — но в сайдбаре по умолчанию
  // показывается только зонтичный пункт, чтобы не перегружать USER-режим.
  { id: 'storages', icon: '▤', staticLabel: 'STORAGES', userI18nKey: 'cloud', modes: ['user', 'dev', 'geek'] },
  // GEEK-only дубликаты для быстрого доступа техно-пользователей.
  // Torrents убран из сайдбара (v1.80.1) — уже доступен через зонтичную
  // страницу Storages, чтобы не дублировать пункт в списке навигации.
  { id: 'music', icon: '♫', i18nKey: 'music', modes: ['geek'] },
  // Карта сети — видна во всех режимах (v1.135.188): в USER-режиме без блока «Моя нода».
  { id: 'network', icon: '⬡', i18nKey: 'network', modes: ['user', 'dev', 'geek'] },
  // AI Computer (sn://ai-computer) — виден ТОЛЬКО адресам из .env DEVELOPERS
  // (как и AI-консоль). Для всех прочих пользователей пункт скрыт.
  { id: 'ai-computer', icon: '⌬', i18nKey: 'aiComputer', gate: 'developers', modes: ['user', 'dev', 'geek'] },
  // Cloud Seeders (ntfryseed): пункт скрыт из сайдбара (v1.122.2) — управление
  // доступно из Settings / Discover; фоновая репликация продолжает работать.
  { id: 'dev', icon: '⎇', i18nKey: 'dev', badgeSource: 'devhub.openPrs', modes: ['dev', 'geek'] },
  // Keys/MyFiles/Settings НЕ показываем в сайдбаре ни в одном режиме
  // (v1.73.0): Keys доступны в Settings, MyFiles — через зонтичную
  // страницу sn://storages, Settings — через иконку ⚙ в топбаре. Это
  // очищает GEEK-сайдбар от дублей и снижает визуальный шум.
]
