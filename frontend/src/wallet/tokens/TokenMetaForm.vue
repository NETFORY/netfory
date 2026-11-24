<script setup lang="ts">
import { computed, defineEmits, defineProps, ref } from 'vue'
import { cachedLogo, cacheLogoFromFile, errKey, readLogo, tokenOp, utf8len, type TokenConfig, type TokenMeta } from './api'
import { useTokT } from './t'
import HoldButton from './HoldButton.vue'
import TokenSummary from './TokenSummary.vue'

// «Витрина» токена (TokenMeta): название, описание, сайт, логотип. Новая отправка заменяет прежние данные.
const props = defineProps<{ id: string; symbol: string; cfg: TokenConfig; initial?: TokenMeta | null; skippable?: boolean }>()
const emit = defineEmits<{ (e: 'done', id: string): void; (e: 'skip'): void }>()
const tt = useTokT()
const name = ref(props.initial?.name ?? '')
const desc = ref(props.initial?.description ?? '')
const site = ref(props.initial?.website ?? '')
const logo = ref<{ type: 'svg' | 'png'; b64: string; dataUrl: string } | null>(null)
const preview = computed(() => logo.value?.dataUrl || cachedLogo(props.id) || '')
const busy = ref(false)
const err = ref('')
const sum = ref<InstanceType<typeof TokenSummary> | null>(null)

async function pick(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0]
  if (!f) return
  const r = await readLogo(f)
  if (typeof r === 'string') { err.value = tt(r); logo.value = null } else { err.value = ''; logo.value = r }
}
const invalid = computed(() => !name.value.trim() || utf8len(name.value) > 64 || utf8len(desc.value) > 512 || utf8len(site.value) > 128 || /\s/.test(site.value.trim()))
async function submit() {
  if (invalid.value || busy.value || sum.value?.low) return
  busy.value = true
  err.value = ''
  const meta: Record<string, string> = { name: name.value.trim(), description: desc.value.trim(), website: site.value.trim() }
  if (logo.value) { meta.logoType = logo.value.type; meta.logo = logo.value.b64 }
  try {
    const tx = await tokenOp('meta', { id: props.id, meta }, props.cfg.fees.meta)
    if (logo.value) cacheLogoFromFile(props.id, logo.value.dataUrl)
    emit('done', tx)
  } catch (e) {
    const k = errKey(e)
    err.value = k.key ? tt(k.key) : k.raw
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="taf" data-testid="token-meta-form">
    <div class="tm-logo">
      <span class="tm-prev"><img v-if="preview" :src="preview" alt="" /><i v-else>◎</i></span>
      <div>
        <label class="tm-pick">{{ tt('pickLogo') }}<input type="file" accept=".svg,.png,image/svg+xml,image/png" @change="pick" hidden data-testid="token-meta-logo" /></label>
        <small class="tk-dim">{{ tt('logoHint') }}</small>
      </div>
    </div>
    <label class="tk-field"><span class="tk-lbl">{{ tt('name') }}</span><input v-model="name" data-testid="token-meta-name" /><small class="tk-dim">{{ tt('bytes', { n: utf8len(name), m: 64 }) }}</small></label>
    <label class="tk-field"><span class="tk-lbl">{{ tt('description') }}</span><textarea v-model="desc" rows="3" data-testid="token-meta-desc" /><small class="tk-dim">{{ tt('bytes', { n: utf8len(desc), m: 512 }) }}</small></label>
    <label class="tk-field"><span class="tk-lbl">{{ tt('website') }}</span><input v-model="site" placeholder="https://" spellcheck="false" data-testid="token-meta-site" /></label>
    <TokenSummary ref="sum" :rows="[[tt('name'), name || '—'], [tt('tokenWord'), symbol]]" :fee="cfg.fees.meta" />
    <p v-if="err" class="taf-err" data-testid="token-meta-error">{{ err }}</p>
    <HoldButton :label="tt('holdPublish')" :disabled="invalid || !cfg.enabled" :busy="busy" @confirm="submit" data-testid="token-meta-submit" />
    <button v-if="skippable" class="tk-skip" @click="emit('skip')" data-testid="token-meta-skip">{{ tt('skipLater') }}</button>
  </div>
</template>

<style scoped>
.taf { display: flex; flex-direction: column; gap: 12px; }
.tm-logo { display: flex; align-items: center; gap: 12px; }
.tm-logo > div { display: flex; flex-direction: column; gap: 6px; }
.tm-prev { width: 56px; height: 56px; flex-shrink: 0; border-radius: 12px; border: 1px solid var(--border); display: inline-flex; align-items: center; justify-content: center; overflow: hidden; color: var(--text-dim); }
.tm-prev img { width: 100%; height: 100%; object-fit: cover; }
.tm-pick { align-self: flex-start; padding: 9px 14px; border-radius: 10px; border: 1px solid var(--border); font-size: 11.5px; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; transition: border-color 0.15s; }
.tm-pick:hover { border-color: var(--accent); }
textarea { padding: 11px 12px; border-radius: 10px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; font-size: 13px; resize: vertical; }
textarea:focus { outline: none; border-color: var(--accent); }
</style>
