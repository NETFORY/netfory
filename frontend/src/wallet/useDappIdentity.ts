import { computed, type Ref } from 'vue'
import { useTabsStore } from '@/store/tabs'

/**
 * Resolve a dApp's human identity (name + trust badge) from its request origin
 * by matching it to an open tab. Origins look like `sth://<appId>` — the appId
 * is the content address used as the tab id. Falls back to the raw host.
 */
export function useDappIdentity(origin: Ref<string | undefined>) {
  const tabs = useTabsStore()
  const appId = computed(() =>
    (origin.value ?? '')
      .replace(/^[a-z]+:\/\//i, '')
      .replace(/\/.*$/, '')
      .trim(),
  )
  const tab = computed(() => tabs.tabs.find((t) => t.id === appId.value) || null)
  const name = computed(() => tab.value?.title || appId.value || 'Unknown dApp')
  // null = unknown/not an open tab, true = verified, false = unverified
  const verified = computed<boolean | null>(() => tab.value?.verified ?? null)
  const host = computed(() => appId.value || 'unknown')
  return { name, verified, host, appId }
}
