/*
 * SmartNet // lib/seederRep.ts
 *
 * Phase 3b: фоновая chain-репликация Cloud Seeder. Раз в 30 минут
 * автоматически переиздаёт стейт сети в папку /ntfryseed пользователя, если:
 *   1) кошелёк разблокирован (unseal vault-конфига проходит);
 *   2) в Crypto Vault подключён Yandex-аккаунт с токеном;
 *   3) пользователь уже публиковал свой сидер (якорь sth_seeder_own).
 * Таймер запускается из AppLayout после разблокировки.
 */
import {
  isTauri,
  cloudSeederPublish,
  cloudSeederDeleteDapp,
  cloudSeederPublishProfiles,
  profileSeederAutoRepGet,
  profileSeederHasChanges,
  profileSeederLastReplicated,
  cloudVaultUnseal,
  cloudVaultYandexRefresh,
  cloudVaultGoogleRefresh,
  type SeederProviderId,
  type SeederPublishResult,
  type ProfilesPublishOut,
} from '@/lib/bridge'
import { useNetStore } from '@/store/net'

export type SeederDriveAccount = {
  id: string
  provider: string
  label: string
  login: string
  token: string
  refresh: string
  expiresAt: number
}

const LS_SEALED = 'ntfry.settings.sec'
const LS_AUTOREP = 'sth_seeder_autorep'
const LS_OWN = 'sth_seeder_own'
const LS_LAST = 'sth_seeder_last_rep'
/** id аккаунта Crypto Vault, выбранного как secondary-зеркало ('' = выкл). */
const LS_SECONDARY = 'sth_seeder_secondary'
const REP_INTERVAL_MS = 60 * 60_000

export function secondaryAccId(): string {
  return localStorage.getItem(LS_SECONDARY) || ''
}
export function setSecondaryAccId(id: string) {
  if (id) localStorage.setItem(LS_SECONDARY, id)
  else localStorage.removeItem(LS_SECONDARY)
}

/** Провайдер по префиксу URI: sn://seed/<y|m|g>@… */
export function provFromUri(uri?: string | null): SeederProviderId {
  if (uri?.startsWith('sn://seed/m@')) return 'mailru'
  if (uri?.startsWith('sn://seed/g@')) return 'gdrive'
  return 'yandex'
}

export function autoRepEnabled(): boolean {
  return localStorage.getItem(LS_AUTOREP) !== '0'
}
export function setAutoRep(on: boolean) {
  localStorage.setItem(LS_AUTOREP, on ? '1' : '0')
}
export function ownSeeder(): { uri: string; publicKey: string } | null {
  try {
    const raw = localStorage.getItem(LS_OWN)
    return raw ? JSON.parse(raw) : null
  } catch {
    return null
  }
}
export function markOwnSeeder(uri: string, publicKey: string) {
  localStorage.setItem(LS_OWN, JSON.stringify({ uri, publicKey }))
  localStorage.setItem(LS_LAST, String(Date.now()))
}
export function lastReplication(): number {
  return Number(localStorage.getItem(LS_LAST) || 0)
}

/** Аккаунты, пригодные для публикации сидера: Yandex/Google (OAuth-токен)
 *  или Mail.ru (token = email:пароль_приложения) из vault-конфига. */
export async function loadSeederAccounts(): Promise<SeederDriveAccount[]> {
  if (!isTauri) return []
  const sealed = localStorage.getItem(LS_SEALED)
  if (!sealed) return []
  try {
    const parsed = JSON.parse(await cloudVaultUnseal(sealed))
    if (!Array.isArray(parsed.accounts)) return []
    return parsed.accounts.filter(
      (a: SeederDriveAccount) =>
        (a.provider === 'yandex' || a.provider === 'mailru' || a.provider === 'gdrive') &&
        !!a.token,
    )
  } catch {
    return [] // кошелёк заблокирован или sealed повреждён — молча
  }
}

/** Свежий токен: у Yandex/Google рефрешим OAuth при истечении,
 *  у Mail.ru креды бессрочны. */
export async function freshSeederToken(acc: SeederDriveAccount): Promise<string> {
  if (acc.provider !== 'yandex' && acc.provider !== 'gdrive') return acc.token
  const soon = Date.now() + 60_000
  if (!acc.refresh || !acc.expiresAt || acc.expiresAt >= soon) return acc.token
  try {
    const r =
      acc.provider === 'gdrive'
        ? await cloudVaultGoogleRefresh(acc.refresh)
        : await cloudVaultYandexRefresh(acc.refresh)
    return (r as any).accessToken || (r as any).token || acc.token
  } catch {
    return acc.token
  }
}

/** Best-effort публикация стейта сети на secondary-зеркало (если выбрано).
 *  null — зеркало не настроено или совпадает с primary. Ошибки — наружу. */
export async function publishSecondary(
  accounts: SeederDriveAccount[],
  primaryId: string,
): Promise<SeederPublishResult | null> {
  const secId = secondaryAccId()
  if (!secId || secId === primaryId) return null
  const acc = accounts.find((a) => a.id === secId)
  if (!acc) return null
  const token = await freshSeederToken(acc)
  return cloudSeederPublish(token, acc.provider as SeederProviderId, undefined, true)
}

/** Best-effort публикация профилей на secondary-зеркало. */
export async function publishProfilesSecondary(
  accounts: SeederDriveAccount[],
  primaryId: string,
): Promise<ProfilesPublishOut | null> {
  const secId = secondaryAccId()
  if (!secId || secId === primaryId) return null
  const acc = accounts.find((a) => a.id === secId)
  if (!acc) return null
  const token = await freshSeederToken(acc)
  return cloudSeederPublishProfiles(token, acc.provider as SeederProviderId, undefined, true)
}

let timer: ReturnType<typeof setInterval> | null = null
let inFlight = false

async function tick(force = false) {
  if (inFlight || !autoRepEnabled() || !ownSeeder()) return
  // offline: OAuth-рефреш и WebDAV-заливка повиснут в таймаутах — пропускаем
  if (!useNetStore().online) return
  if (!force && Date.now() - lastReplication() < REP_INTERVAL_MS) return
  inFlight = true
  try {
    const accounts = await loadSeederAccounts()
    if (!accounts.length) return
    // фоновая репликация идёт на тот же провайдер, где опубликован свой сидер
    const wantProv = provFromUri(ownSeeder()?.uri)
    const acc = accounts.find((a) => a.provider === wantProv) || accounts[0]
    const token = await freshSeederToken(acc)
    const res = await cloudSeederPublish(token, acc.provider as SeederProviderId)
    markOwnSeeder(res.uri, res.publicKey)
    window.dispatchEvent(new CustomEvent('seeder-replicated', { detail: res }))
    // secondary-зеркало: ошибка НЕ валит успешную публикацию primary
    try {
      const res2 = await publishSecondary(accounts, acc.id)
      if (res2) window.dispatchEvent(new CustomEvent('seeder-secondary-replicated', { detail: res2 }))
    } catch (e2) {
      console.warn('[seeder] secondary replication failed:', e2)
      window.dispatchEvent(
        new CustomEvent('seeder-secondary-error', { detail: String((e2 as any)?.message || e2) }),
      )
    }
  } catch (e) {
    console.warn('[seeder] background replication failed:', e)
  } finally {
    inFlight = false
  }
}

/** Запуск фоновой репликации (AppLayout, после разблокировки). */
export function startSeederReplication() {
  if (!isTauri || timer) return
  timer = setInterval(() => {
    void tick()
  }, 60_000)
  void tick()
}

/** Мгновенная репликация после публикации/обновления dApp (не ждём 30 мин).
 *  Уважает тумблер авто-репликации и требует ранее опубликованный сидер. */
export function replicateNow() {
  void tick(true)
}

/** Пользователь удалил dApp локально → удаляем его архив и со СВОЕГО cloud
 *  seed диска, затем сразу переиздаём стейт (dapps.enc без удалённого id).
 *  Фоново и best-effort: нет своего сидера / нет токена — молча выходим. */
export async function deleteDappFromCloud(appId: string) {
  if (!isTauri || !ownSeeder()) return
  try {
    const accounts = await loadSeederAccounts()
    if (!accounts.length) return
    const wantProv = provFromUri(ownSeeder()?.uri)
    const acc = accounts.find((a) => a.provider === wantProv) || accounts[0]
    const token = await freshSeederToken(acc)
    await cloudSeederDeleteDapp(token, acc.provider as SeederProviderId, appId)
    // зеркальный диск: тоже подчищаем архив (best-effort)
    const secId = secondaryAccId()
    if (secId && secId !== acc.id) {
      const sacc = accounts.find((a) => a.id === secId)
      if (sacc) {
        try {
          const stoken = await freshSeederToken(sacc)
          await cloudSeederDeleteDapp(stoken, sacc.provider as SeederProviderId, appId)
        } catch (e2) {
          console.warn('[seeder] удаление dApp с зеркала не удалось:', e2)
        }
      }
    }
    replicateNow()
  } catch (e) {
    console.warn('[seeder] удаление dApp с cloud seed диска не удалось:', e)
  }
}

export function stopSeederReplication() {
  if (timer) {
    clearInterval(timer)
    timer = null
  }
  if (profileTimer) {
    clearInterval(profileTimer)
    profileTimer = null
  }
}

// ── Public Profile Seeder — фоновая репликация профилей (раз в час) ────────

/** Интервал = 1 час: профили меняются реже, чем состояние сети. */
const PROFILE_REP_INTERVAL_MS = 60 * 60_000

let profileTimer: ReturnType<typeof setInterval> | null = null
let profileInFlight = false

async function profileTick(force = false) {
  if (profileInFlight) return
  // offline: облака недоступны — пропускаем тик
  if (!useNetStore().online) return
  if (!(await profileSeederAutoRepGet())) return
  if (!ownSeeder()) return // без URI сидера нам некуда/незачем лить
  if (!force) {
    const last = await profileSeederLastReplicated()
    if (Date.now() - last < PROFILE_REP_INTERVAL_MS) return
  }
  // Дешёвый предварительный чек — если ничего не изменилось, вообще
  // не идём в облако (0 сетевых запросов).
  if (!force && !(await profileSeederHasChanges())) return
  profileInFlight = true
  try {
    const accounts = await loadSeederAccounts()
    if (!accounts.length) return
    const wantProv = provFromUri(ownSeeder()?.uri)
    const acc = accounts.find((a) => a.provider === wantProv) || accounts[0]
    const token = await freshSeederToken(acc)
    const res = await cloudSeederPublishProfiles(token, acc.provider as SeederProviderId)
    window.dispatchEvent(new CustomEvent('profile-seeder-replicated', { detail: res }))
    try {
      await publishProfilesSecondary(accounts, acc.id)
    } catch (e2) {
      console.warn('[profile-seeder] secondary replication failed:', e2)
      window.dispatchEvent(
        new CustomEvent('seeder-secondary-error', { detail: String((e2 as any)?.message || e2) }),
      )
    }
  } catch (e) {
    console.warn('[profile-seeder] background replication failed:', e)
  } finally {
    profileInFlight = false
  }
}

/** Запуск фоновой репликации профилей (AppLayout, после разблокировки). */
export function startProfileReplication() {
  if (!isTauri || profileTimer) return
  profileTimer = setInterval(() => {
    void profileTick()
  }, 5 * 60_000) // проверяем каждые 5 минут, реально льём — раз в час, если есть изменения
  void profileTick()
}

/** Мгновенная репликация профилей после локальной публикации поста и т.д. */
export function replicateProfilesNow() {
  void profileTick(true)
}
