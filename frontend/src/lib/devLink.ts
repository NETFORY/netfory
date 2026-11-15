// dev:// → внутренний маршрут Vue Router. Общая точка для deep-link (App.vue)
// и внутренних кликов по dev://-ссылкам.
import type { Router } from 'vue-router'
import { parseDevUrl, type DevTarget } from '@/lib/bridge'

export function devTargetToRoute(t: DevTarget) {
  if (t.target === 'user_profile') {
    return { name: 'dev-user', params: { username: t.username } }
  }
  return {
    name: 'dev-repo',
    params: { username: t.username, repo: t.repo },
    query: t.subsystem && t.subsystem !== 'code' ? { tab: t.subsystem } : {},
  }
}

/** Разобрать dev://-URL и смаршрутизировать. Невалидный → хаб с флагом ошибки. */
export async function openDevUrl(router: Router, url: string): Promise<void> {
  try {
    const t = await parseDevUrl(url)
    await router.push(devTargetToRoute(t))
  } catch {
    await router.push({ name: 'dev', query: { error: '1', url } })
  }
}
