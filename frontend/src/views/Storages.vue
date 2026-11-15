<script setup lang="ts">
/*
 * SmartNet // views/Storages.vue
 *
 * Зонтичная страница «Хранилища & Медиа» — единая точка входа в разделы
 * Торренты / Мои файлы / Музыка. Задача — уменьшить когнитивную нагрузку
 * левого сайдбара в USER-режиме: один пункт «Хранилища» вместо трёх, а
 * дальше пользователь сам ныряет в нужный тип хранилища через большие
 * акцентные карточки в стиле Dashboard.
 *
 * Дополнительно (этап G): горизонтальный «pie-bar» вверху страницы —
 * визуальный расклад «что жрёт диск» по категориям тегов файлов
 * (Music / Dev / Images / Other). Данные берутся из `listMyFiles()` +
 * `parseFileTag(tag)` — та же логика, что и на странице MyFiles.
 */
import { onMounted, ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import { listMyFiles, type FileManifest } from '@/lib/bridge'
import { parseFileTag } from '@/lib/fileTags'
import FirstVisitHint from '@/components/FirstVisitHint.vue'

const router = useRouter()

// Загружаем мои файлы для storage-pie (best-effort — если сервер вне
// Tauri, просто ничего не рисуем).
const myFiles = ref<FileManifest[]>([])
onMounted(async () => {
  try { myFiles.value = await listMyFiles() } catch { /* preview / no tauri */ }
})

type Slice = { id: 'music' | 'dev' | 'image' | 'other'; label: string; color: string; count: number; size: number }
const usage = computed<{ slices: Slice[]; total: number }>(() => {
  const box: Record<Slice['id'], Slice> = {
    music: { id: 'music', label: 'Music', color: '#f5b301', count: 0, size: 0 },
    dev:   { id: 'dev',   label: 'Dev',   color: '#60a5fa', count: 0, size: 0 },
    image: { id: 'image', label: 'Images',color: '#22c55e', count: 0, size: 0 },
    other: { id: 'other', label: 'Other', color: '#a78bfa', count: 0, size: 0 },
  }
  for (const f of myFiles.value) {
    const c = parseFileTag(f.tag).cat
    const id: Slice['id'] = c === 'music' || c === 'dev' || c === 'image' ? c : 'other'
    box[id].count++
    box[id].size += f.size || 0
  }
  const slices = (Object.values(box) as Slice[]).filter((s) => s.size > 0)
  const total = slices.reduce((s, x) => s + x.size, 0)
  return { slices, total }
})

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}

// Карточки хранилищ. `route` — имя роута (v-router). Кастомные глифы
// (не эмодзи), чтобы стилистически совпадать с сайдбаром и Dashboard.
const cards = [
  {
    id: 'torrents',
    glyph: '⇅',
    title: 'Torrents',
    tagline: 'BitTorrent downloads',
    desc: 'Classic BitTorrent client with DHT: download, seed, manage the queue and rate limits.',
    route: 'torrents',
    hue: 'orange',
  },
  {
    id: 'myfiles',
    glyph: '⌸',
    title: 'My Files',
    tagline: 'Publish & seed',
    desc: 'Your published files: E2EE encryption, paid private shares, sales stats.',
    route: 'myfiles',
    hue: 'green',
  },
  {
    id: 'music',
    glyph: '♫',
    title: 'Music',
    tagline: 'P2P player & albums',
    desc: 'Local music library and Web4 player - buy albums straight from artists, no middlemen.',
    route: 'music',
    hue: 'blue',
  },
] as const

function open(route: string) {
  router.push({ name: route })
}
</script>

<template>
  <div class="storages" data-testid="storages-view">
    <header class="st-head">
      <h1 class="st-title">// STORAGES & MEDIA_</h1>
      <p class="st-sub">
        One screen for every kind of local storage you keep. Data lives on your own disk
        and is seeded by you — no clouds, no servers, no subscriptions.
      </p>
    </header>

    <!-- Подсказка новичку при первом открытии облака -->
    <FirstVisitHint storage-key="sth_hint_cloud_seen" title-key="hints.cloudTitle" text-key="hints.cloudText" />

    <!-- Storage-usage bar (этап G): горизонтальный «pie», подпись под ним
         с легендой и итоговым размером. Показываем только если у пользователя
         действительно есть опубликованные файлы. -->
    <section v-if="usage.total > 0" class="st-usage" data-testid="storages-usage">
      <div class="st-usage-head">
        <span class="st-usage-title mono-label">[ Disk usage ]</span>
        <span class="st-usage-total">{{ fmtBytes(usage.total) }} total</span>
      </div>
      <div class="st-usage-bar" role="img" aria-label="Storage usage by category">
        <span
          v-for="s in usage.slices"
          :key="s.id"
          class="st-usage-seg"
          :style="{ width: `${(s.size / usage.total) * 100}%`, background: s.color }"
          :title="`${s.label}: ${s.count} · ${fmtBytes(s.size)}`"
          :data-testid="`storages-usage-seg-${s.id}`"
        />
      </div>
      <div class="st-usage-legend">
        <span
          v-for="s in usage.slices"
          :key="s.id"
          class="st-usage-chip"
          :data-testid="`storages-usage-chip-${s.id}`"
        >
          <span class="st-usage-dot" :style="{ background: s.color }" />
          {{ s.label }} · <b>{{ s.count }}</b> · {{ fmtBytes(s.size) }}
        </span>
      </div>
    </section>

    <div class="st-grid">
      <button
        v-for="c in cards"
        :key="c.id"
        class="st-card"
        :data-hue="c.hue"
        :data-testid="`storages-card-${c.id}`"
        @click="open(c.route)"
      >
        <span class="st-glyph" aria-hidden="true">{{ c.glyph }}</span>
        <div class="st-body">
          <span class="st-tag mono-label">[ {{ c.tagline }} ]</span>
          <h2 class="st-cardtitle">{{ c.title }}</h2>
          <p class="st-desc">{{ c.desc }}</p>
        </div>
        <span class="st-arrow" aria-hidden="true">→</span>
      </button>

      <!-- Crypto Vault (ntfry): терминальная карточка Zero-Knowledge хранилища -->
      <button class="st-vault" data-testid="storages-card-vault" @click="open('vault')">
        <span class="sv-tag">[ VIRTUAL ENCRYPTED VAULT ]</span>
        <span class="sv-title">Secure Cloud Drive</span>
        <span class="sv-desc">Zero-Knowledge container stored on<br />Google Drive / Yandex Disk. E2EE + ZSTD.&nbsp;<span class="sv-arrow">→</span></span>
      </button>

      <!-- Cloud Seeders (ntfryseed): бирюзовая карточка публичного анти-цензурного слоя -->
      <button class="st-seeder" data-testid="storages-card-seeders" @click="open('seeders')">
        <span class="sd-tag">[ PUBLIC NETWORK SEEDER ]</span>
        <span class="sd-title">Cloud Seeders</span>
        <span class="sd-desc">Публикация подписанного стейта сети<br />(dApps · nodes) в открытую папку. HKDF · secp256k1.&nbsp;<span class="sd-arrow">→</span></span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.storages {
  padding: 24px 32px 48px;
  max-width: 1280px;
  margin: 0 auto;
}
.st-head { margin-bottom: 32px; }
.st-title {
  font-family: var(--font-mono);
  font-size: 22px;
  font-weight: var(--fw-700);
  letter-spacing: 0.08em;
  color: var(--text);
  margin: 0 0 8px;
}
.st-sub {
  font-size: 14px;
  color: var(--text-dim);
  max-width: 640px;
  margin: 0;
  line-height: 1.6;
}

/* ── Storage usage bar (этап G) ─────────────────────────────────────
   Горизонтальный «pie», подкладываем как первое ощущение от страницы:
   пользователь сразу видит, что 60% диска — музыка, 20% dev-релизы и
   т.д. Каждый сегмент имеет свой цвет-акцент под категорию. */
.st-usage {
  margin-bottom: 32px;
  padding: 18px 20px 16px;
  background: var(--surface, var(--panel));
  border: 1px solid var(--border);
  border-radius: 14px;
}
.st-usage-head {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 10px;
}
.st-usage-title {
  font-size: 10.5px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
}
.st-usage-total {
  font-family: var(--font-mono);
  font-size: 12.5px;
  font-weight: var(--fw-700);
  color: var(--text);
  letter-spacing: 0.04em;
}
.st-usage-bar {
  display: flex;
  width: 100%;
  height: 10px;
  border-radius: 999px;
  overflow: hidden;
  background: light-dark(rgba(0,0,0,0.06), rgba(255,255,255,0.04));
  border: 1px solid var(--border);
  margin-bottom: 12px;
}
.st-usage-seg {
  height: 100%;
  transition: filter 0.15s ease;
}
.st-usage-seg:hover { filter: brightness(1.25); }
.st-usage-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}
.st-usage-chip {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 4px 12px;
  border-radius: 999px;
  background: light-dark(rgba(0,0,0,0.05), rgba(0,0,0,0.28));
  border: 1px solid var(--border);
  color: var(--text-dim);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.03em;
}
.st-usage-chip b { color: var(--text); }
.st-usage-dot {
  width: 8px; height: 8px; border-radius: 50%;
  box-shadow: 0 0 6px currentColor;
}

.st-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 20px;
}

.st-card {
  position: relative;
  display: grid;
  grid-template-columns: 64px 1fr auto;
  align-items: center;
  gap: 20px;
  padding: 24px 24px 24px 20px;
  background: var(--surface, var(--panel));
  border: 1px solid var(--border);
  border-radius: 16px;
  text-align: left;
  cursor: pointer;
  color: var(--text);
  transition: transform 0.18s ease, border-color 0.18s ease, background-color 0.18s ease;
  overflow: hidden;
}
.st-card::before {
  content: '';
  position: absolute; inset: 0;
  background: radial-gradient(circle at 12% 20%, var(--_hue, transparent) 0%, transparent 45%);
  opacity: 0.14;
  pointer-events: none;
  transition: opacity 0.2s ease;
}
.st-card[data-hue='orange'] { --_hue: #f59e0b; }
.st-card[data-hue='green']  { --_hue: light-dark(#15803d, #22c55e); }
.st-card[data-hue='blue']   { --_hue: light-dark(#2563eb, #60a5fa); }
.st-card:hover {
  transform: translateY(-3px);
  border-color: var(--_hue, var(--accent));
}
.st-card:hover::before { opacity: 0.28; }

.st-glyph {
  font-size: 40px;
  line-height: 1;
  color: var(--_hue, var(--text));
  text-shadow: 0 0 24px var(--_hue, transparent);
  filter: drop-shadow(0 0 10px var(--_hue, transparent));
}

.st-body { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.st-tag {
  font-size: 10px;
  letter-spacing: 0.14em;
  color: var(--text-dim);
  opacity: 0.85;
}
.st-cardtitle {
  font-size: 20px;
  font-weight: var(--fw-700);
  margin: 0;
  color: var(--text);
}
.st-desc {
  font-size: 13px;
  color: var(--text-dim);
  margin: 0;
  line-height: 1.55;
}

.st-arrow {
  font-size: 22px;
  color: var(--_hue, var(--text-dim));
  transition: transform 0.18s ease;
  padding-left: 12px;
}
.st-card:hover .st-arrow { transform: translateX(6px); }

/* ── Crypto Vault (ntfry) — терминальный стиль под ASCII-макет ───────── */
.st-vault {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
  padding: 22px 24px;
  background: light-dark(var(--panel), rgba(0, 0, 0, 0.32));
  border: 1px solid rgba(167, 139, 250, 0.45);
  border-radius: 6px;
  text-align: left;
  cursor: pointer;
  font-family: var(--font-mono);
  color: var(--text);
  transition: transform 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
}
.st-vault:hover {
  transform: translateY(-3px);
  border-color: light-dark(#7c3aed, #a78bfa);
  box-shadow: 0 0 24px rgba(167, 139, 250, 0.18);
}
.sv-tag {
  font-size: 11px;
  letter-spacing: 0.14em;
  color: light-dark(#7c3aed, #a78bfa);
}
.sv-title {
  font-size: 18px;
  font-weight: var(--fw-700);
  letter-spacing: 0.02em;
}
.sv-desc {
  font-size: 12.5px;
  color: var(--text-dim);
  line-height: 1.6;
}
.sv-arrow { color: light-dark(#7c3aed, #a78bfa); transition: margin-left 0.18s ease; }
.st-vault:hover .sv-arrow { margin-left: 6px; }

/* ── Cloud Seeders (ntfryseed) — бирюзовая терминальная карточка ─────── */
.st-seeder {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
  padding: 22px 24px;
  background: light-dark(var(--panel), rgba(0, 0, 0, 0.32));
  border: 1px solid rgba(38, 208, 206, 0.45);
  border-radius: 6px;
  text-align: left;
  cursor: pointer;
  font-family: var(--font-mono);
  color: var(--text);
  transition: transform 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
}
.st-seeder:hover {
  transform: translateY(-3px);
  border-color: light-dark(#0d9488, #26d0ce);
  box-shadow: 0 0 24px rgba(38, 208, 206, 0.22);
}
.sd-tag {
  font-size: 11px;
  letter-spacing: 0.14em;
  color: light-dark(#0d9488, #26d0ce);
}
.sd-title {
  font-size: 18px;
  font-weight: var(--fw-700);
  letter-spacing: 0.02em;
}
.sd-desc {
  font-size: 12.5px;
  color: var(--text-dim);
  line-height: 1.6;
}
.sd-arrow { color: light-dark(#0d9488, #26d0ce); transition: margin-left 0.18s ease; }
.st-seeder:hover .sd-arrow { margin-left: 6px; }
</style>
