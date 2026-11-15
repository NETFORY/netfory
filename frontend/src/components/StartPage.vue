<script setup lang="ts">
// Стартовая страница новой вкладки: поиск по SmartNet (dApps, имена u://, сайты,
// которые вы раздаёте/сохранили) с запасным вариантом — Google.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDiscoveryStore } from '@/store/discovery'
import { useBookmarksStore } from '@/store/bookmarks'
import { listUsernames, type NameEntry } from '@/lib/bridge'

const emit = defineEmits<{ open: [input: string] }>()
const { t } = useI18n()
const disc = useDiscoveryStore()
const bookmarks = useBookmarksStore()

const q = ref('')
const names = ref<NameEntry[]>([])
const input = ref<HTMLInputElement | null>(null)
const cursor = ref(0)

onMounted(async () => {
  input.value?.focus()
  names.value = await listUsernames().catch(() => [])
})

const norm = (s: string) => s.toLowerCase().trim()
const hit = (hay: string, needle: string) => norm(hay).includes(needle)

interface Row { key: string; kind: 'dapp' | 'seed' | 'name' | 'site' | 'google'; title: string; sub: string; input: string }

const rows = computed<Row[]>(() => {
  const n = norm(q.value)
  const out: Row[] = []
  if (n) {
    const apps = disc.apps.filter((a) => hit(a.title, n) || hit(a.description, n) || a.tags.some((x) => hit(x, n)) || hit(a.id, n))
    apps.filter((a) => a.installed).slice(0, 4).forEach((a) => out.push({ key: `seed:${a.id}`, kind: 'seed', title: a.title || a.id, sub: a.description, input: `sth://${a.id}` }))
    apps.filter((a) => !a.installed).sort((x, y) => (y.seeds ?? 0) - (x.seeds ?? 0)).slice(0, 5)
      .forEach((a) => out.push({ key: `dapp:${a.id}`, kind: 'dapp', title: a.title || a.id, sub: `${a.description} · ${a.seeds} seeds`, input: `sth://${a.id}` }))
    names.value.filter((u) => hit(u.name, n)).slice(0, 5)
      .forEach((u) => out.push({ key: `name:${u.name}`, kind: 'name', title: `u://${u.name}`, sub: u.address, input: `u://${u.name}` }))
    bookmarks.items.filter((b) => hit(b.title, n) || hit(b.url, n)).slice(0, 5)
      .forEach((b) => out.push({ key: `site:${b.url}`, kind: 'site', title: b.title || b.url, sub: b.url, input: b.url }))
    if (/^https:\/\/\S+$/i.test(q.value.trim())) out.push({ key: 'url', kind: 'site', title: q.value.trim(), sub: t('start.openUrl'), input: q.value.trim() })
  }
  const google = `https://www.google.com/search?q=${encodeURIComponent(q.value.trim())}`
  out.push({ key: 'google', kind: 'google', title: n ? t('start.google', { q: q.value.trim() }) : t('start.googleHome'), sub: 'google.com', input: n ? google : 'https://www.google.com/' })
  return out
})
watch(rows, () => { cursor.value = 0 })

// Без запроса: что вы раздаёте и ваши закладки — быстрый старт как в Chrome
const seeded = computed(() => disc.apps.filter((a) => a.installed).slice(0, 8))
const saved = computed(() => bookmarks.items.slice(0, 16))

function go(r: Row) { emit('open', r.input) }
function onEnter() { const r = rows.value[cursor.value]; if (r) go(r) }
function move(d: number) { cursor.value = (cursor.value + d + rows.value.length) % rows.value.length }
const kindLabel = (k: Row['kind']) => t(`start.kind.${k}`)
</script>

<template>
  <div class="start" data-testid="start-page">
    <div class="start-logo">NET<span>FORY</span><small>{{ t('start.tagline') }}</small></div>
    <div class="start-box">
      <input
        ref="input"
        v-model="q"
        class="start-input"
        :placeholder="t('start.placeholder')"
        spellcheck="false"
        data-testid="start-search-input"
        @keydown.enter.prevent="onEnter"
        @keydown.down.prevent="move(1)"
        @keydown.up.prevent="move(-1)"
      />
      <ul v-if="q.trim()" class="start-results" data-testid="start-results">
        <li
          v-for="(r, i) in rows"
          :key="r.key"
          :class="['start-row', `k-${r.kind}`, { sel: i === cursor }]"
          :data-testid="`start-row-${r.kind}`"
          @mouseenter="cursor = i"
          @click="go(r)"
        >
          <span class="k">{{ kindLabel(r.kind) }}</span>
          <span class="ttl">{{ r.title }}</span>
          <span class="sub">{{ r.sub }}</span>
        </li>
      </ul>
    </div>
    <div v-if="!q.trim()" class="start-quick">
      <section v-if="seeded.length">
        <h3>{{ t('start.seeded') }}</h3>
        <div class="chips">
          <button v-for="a in seeded" :key="a.id" class="chip" :title="a.description" @click="emit('open', `sth://${a.id}`)" :data-testid="`start-seed-${a.id}`">{{ a.title || a.id.slice(0, 10) }}</button>
        </div>
      </section>
      <section v-if="saved.length">
        <h3>{{ t('start.saved') }}</h3>
        <div class="chips">
          <button v-for="b in saved" :key="b.url" class="chip site" :title="b.url" @click="emit('open', b.url)" data-testid="start-bookmark">{{ b.title || b.url }}</button>
        </div>
      </section>
      <p class="start-hint">{{ t('start.hint') }}</p>
    </div>
  </div>
</template>

<style scoped>
.start { height: 100%; overflow: auto; display: flex; flex-direction: column; align-items: center; padding: 11vh 24px 40px; gap: 28px; background: radial-gradient(ellipse at 50% 0%, rgba(45, 212, 191, 0.08), transparent 60%), var(--bg); }
.start-logo { font-size: 42px; font-weight: 800; letter-spacing: 0.14em; color: var(--text); display: flex; align-items: baseline; gap: 10px; }
.start-logo span { color: var(--accent, #2dd4bf); }
.start-logo small { font-size: 11px; font-weight: 500; letter-spacing: 0.18em; color: var(--text-dim); text-transform: uppercase; }
.start-box { width: min(720px, 100%); position: relative; }
.start-input { width: 100%; padding: 16px 22px; font: inherit; font-size: 16px; color: var(--text); background: var(--bg-elevated); border: 1px solid var(--border); border-radius: 28px; outline: none; transition: border-color 0.16s ease, box-shadow 0.16s ease; }
.start-input:focus { border-color: var(--accent, #2dd4bf); box-shadow: 0 0 0 4px rgba(45, 212, 191, 0.12); }
.start-results { list-style: none; margin: 8px 0 0; padding: 6px; background: var(--bg-elevated); border: 1px solid var(--border); border-radius: 16px; }
.start-row { display: grid; grid-template-columns: 84px 1fr; column-gap: 12px; padding: 9px 12px; border-radius: 10px; cursor: pointer; }
.start-row.sel { background: rgba(45, 212, 191, 0.1); }
.start-row .k { grid-row: span 2; align-self: center; font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--text-dim); border: 1px solid var(--border); border-radius: 999px; padding: 3px 0; text-align: center; }
.start-row.k-seed .k, .start-row.k-dapp .k { color: var(--accent, #2dd4bf); border-color: rgba(45, 212, 191, 0.4); }
.start-row.k-name .k { color: #a78bfa; border-color: rgba(167, 139, 250, 0.4); }
.start-row.k-google .k { color: #fbbf24; border-color: rgba(251, 191, 36, 0.4); }
.start-row .ttl { font-size: 14px; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.start-row .sub { font-size: 11px; color: var(--text-dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.start-quick { width: min(720px, 100%); display: flex; flex-direction: column; gap: 18px; }
.start-quick h3 { margin: 0 0 8px; font-size: 11px; letter-spacing: 0.16em; text-transform: uppercase; color: var(--text-dim); }
.chips { display: flex; flex-wrap: wrap; gap: 8px; }
.chip { padding: 8px 14px; font: inherit; font-size: 12px; color: var(--text); background: var(--bg-elevated); border: 1px solid var(--border); border-radius: 999px; cursor: pointer; transition: border-color 0.16s ease, transform 0.16s ease; max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.chip:hover { border-color: var(--accent, #2dd4bf); transform: translateY(-1px); }
.chip.site { color: var(--text-dim); }
.start-hint { margin: 0; font-size: 11px; color: var(--text-dim); letter-spacing: 0.04em; }
</style>
