<script setup lang="ts">
// Создание нового репозитория — аналог GitHub «Create a new repository».
// Бесплатно: публикация подписывается ключом кошелька, транзакций нет.
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { devhubCreateRepo, getMyUsername, getUserRepositories } from '@/lib/bridge'
import { useToastsStore } from '@/store/toasts'

const router = useRouter()
const toasts = useToastsStore()

const myUsername = ref('')
const loadingUser = ref(true)
const existing = ref<string[]>([])

const name = ref('')
const description = ref('')
const addReadme = ref(true)
const license = ref('MIT')
const creating = ref(false)
const error = ref('')

const LICENSES = [
  { id: '', label: 'No license' },
  { id: 'MIT', label: 'MIT License' },
  { id: 'Apache-2.0', label: 'Apache License 2.0' },
  { id: 'GPL-3.0', label: 'GNU GPL v3.0' },
  { id: 'BSD-3-Clause', label: 'BSD 3-Clause' },
  { id: 'Unlicense', label: 'The Unlicense' },
]

const nameValid = computed(() => /^[A-Za-z0-9._-]+$/.test(name.value.trim()))
const nameTaken = computed(() => existing.value.includes(name.value.trim()))
const nameHint = computed(() => {
  const n = name.value.trim()
  if (!n) return ''
  if (!nameValid.value) return 'Only Latin letters, digits and . _ - are allowed'
  if (nameTaken.value) return `Repository «${n}» already exists`
  return ''
})
const canCreate = computed(
  () => !!myUsername.value && !!name.value.trim() && nameValid.value && !nameTaken.value && !creating.value,
)

onMounted(async () => {
  myUsername.value = (await getMyUsername()) || ''
  if (myUsername.value) {
    try {
      const u = await getUserRepositories(myUsername.value)
      existing.value = u.repositories.map((r) => r.name)
    } catch {
      existing.value = []
    }
  }
  loadingUser.value = false
})

async function create() {
  if (!canCreate.value) return
  creating.value = true
  error.value = ''
  try {
    const meta = await devhubCreateRepo(name.value.trim(), description.value.trim(), license.value, addReadme.value)
    toasts.push('dev://', `Repository «${meta.name}» created and announced to the network`, 'in')
    router.push({ name: 'dev-repo', params: { username: myUsername.value, repo: meta.name } })
  } catch (e: any) {
    error.value = typeof e === 'string' ? e : e?.message || 'Failed to create repository'
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <div class="dev-new" data-testid="dev-new-repo">
    <button class="back" @click="router.push({ name: 'dev' })" data-testid="dev-new-back">‹ dev:// hub</button>

    <h1 class="dn-title">Create a new repository</h1>
    <p class="dn-sub dim">
      The repository is served from your account into the P2P network and signed with your wallet key.
      Publishing is <b>free</b> — no blockchain transactions required.
    </p>

    <p v-if="loadingUser" class="dim">Checking your dev:// name…</p>

    <div v-else-if="!myUsername" class="dn-warn" data-testid="dev-new-no-username">
      <p>To publish repositories you need an on-chain username — it becomes your address
        <code class="mono">dev://name</code>.</p>
      <p class="dim">Register a name in Profile → «Username», then come back here.</p>
    </div>

    <template v-else>
      <!-- Основное -->
      <section class="dn-card">
        <h2 class="dn-h2">Basics</h2>
        <div class="dn-owner-row">
          <div class="dn-field">
            <label class="dn-label">Owner *</label>
            <span class="dn-owner mono" data-testid="dev-new-owner">@{{ myUsername }}</span>
          </div>
          <span class="dn-slash">/</span>
          <div class="dn-field grow">
            <label class="dn-label" for="repo-name">Repository name *</label>
            <input
              id="repo-name"
              v-model="name"
              class="dn-input mono"
              type="text"
              placeholder="my-awesome-project"
              maxlength="64"
              data-testid="dev-new-name-input"
            />
          </div>
        </div>
        <p v-if="nameHint" class="dn-err" data-testid="dev-new-name-hint">{{ nameHint }}</p>

        <div class="dn-field">
          <label class="dn-label" for="repo-desc">Description <span class="dim">(optional)</span></label>
          <input
            id="repo-desc"
            v-model="description"
            class="dn-input"
            type="text"
            maxlength="350"
            placeholder="Short project description…"
            data-testid="dev-new-desc-input"
          />
          <span class="dn-counter dim">{{ description.length }} / 350</span>
        </div>
      </section>

      <!-- Конфигурация -->
      <section class="dn-card">
        <h2 class="dn-h2">Configuration</h2>

        <div class="dn-row">
          <div>
            <p class="dn-row-title">Visibility</p>
            <p class="dim dn-row-sub">Who can see and clone the repository</p>
          </div>
          <div class="dn-vis">
            <span class="dn-badge pub" data-testid="dev-new-visibility">Public</span>
            <span class="dn-badge soon" title="Private repositories (E2EE) coming later">Private — soon</span>
          </div>
        </div>

        <div class="dn-row">
          <div>
            <p class="dn-row-title">Add README</p>
            <p class="dim dn-row-sub">Starter README.md with the name and description</p>
          </div>
          <label class="dn-toggle">
            <input v-model="addReadme" type="checkbox" data-testid="dev-new-readme-toggle" />
            <span class="dn-toggle-track"><span class="dn-toggle-thumb" /></span>
          </label>
        </div>

        <div class="dn-row">
          <div>
            <p class="dn-row-title">License</p>
            <p class="dim dn-row-sub">LICENSE.md will be created automatically</p>
          </div>
          <select v-model="license" class="dn-select" data-testid="dev-new-license-select">
            <option v-for="l in LICENSES" :key="l.id" :value="l.id">{{ l.label }}</option>
          </select>
        </div>
      </section>

      <p v-if="error" class="dn-err" data-testid="dev-new-error">{{ error }}</p>

      <button class="dn-create" :disabled="!canCreate" @click="create" data-testid="dev-new-create-btn">
        {{ creating ? 'Creating…' : 'Create repository' }}
      </button>
    </template>
  </div>
</template>

<style scoped>
.dev-new { padding: 28px 32px; max-width: 760px; margin: 0 auto; height: 100%; overflow-y: auto; }
.mono { font-family: var(--font-mono, monospace); }
.dim { color: var(--text-dim); }
.back { font-size: 12px; color: var(--text-dim); margin-bottom: 18px; cursor: pointer; background: none; border: none; }
.back:hover { color: var(--accent); }
.dn-title { font-size: 24px; font-weight: var(--fw-800); }
.dn-sub { font-size: 13px; margin: 8px 0 22px; line-height: 1.6; }
.dn-warn { padding: 18px 20px; border-radius: 12px; border: 1px dashed var(--border-accent); font-size: 13px; line-height: 1.7; }
.dn-warn code { color: var(--accent); }
.dn-card {
  padding: 18px 20px; border-radius: 14px; margin-bottom: 16px;
  background: rgba(255,255,255,0.02); border: 1px solid var(--border);
}
.dn-h2 { font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--secondary); font-weight: var(--fw-700); margin-bottom: 14px; }
.dn-owner-row { display: flex; align-items: flex-end; gap: 10px; }
.dn-slash { font-size: 20px; color: var(--text-dim); padding-bottom: 8px; }
.dn-field { display: flex; flex-direction: column; gap: 6px; position: relative; margin-bottom: 12px; }
.dn-field.grow { flex: 1; }
.dn-label { font-size: 12px; font-weight: var(--fw-700); }
.dn-owner {
  display: inline-block; padding: 9px 14px; border-radius: 10px; font-size: 13px;
  background: var(--bg-elevated); border: 1px solid var(--border); color: var(--secondary);
}
.dn-input {
  padding: 9px 12px; border-radius: 10px; font-size: 13px;
  background: var(--bg-elevated); border: 1px solid var(--border); color: var(--text); outline: none;
  transition: border-color 0.15s ease;
}
.dn-input:focus { border-color: var(--border-accent); }
.dn-counter { position: absolute; right: 4px; bottom: -16px; font-size: 10px; }
.dn-err { color: #f87171; font-size: 12px; margin: 6px 0 10px; }
.dn-row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 10px 0; border-top: 1px solid var(--border); }
.dn-row:first-of-type { border-top: none; }
.dn-row-title { font-size: 13px; font-weight: var(--fw-700); }
.dn-row-sub { font-size: 11px; margin-top: 2px; }
.dn-vis { display: flex; gap: 8px; align-items: center; }
.dn-badge { padding: 4px 12px; border-radius: 999px; font-size: 11px; border: 1px solid var(--border); }
.dn-badge.pub { color: #34d399; border-color: rgba(52, 211, 153, 0.4); }
.dn-badge.soon { color: var(--text-dim); }
.dn-select {
  padding: 8px 12px; border-radius: 10px; font-size: 13px;
  background: var(--bg-elevated); border: 1px solid var(--border); color: var(--text); outline: none; cursor: pointer;
}
.dn-toggle { position: relative; display: inline-block; cursor: pointer; }
.dn-toggle input { position: absolute; opacity: 0; }
.dn-toggle-track {
  display: block; width: 40px; height: 22px; border-radius: 999px;
  background: var(--bg-elevated); border: 1px solid var(--border); transition: background-color 0.15s ease;
}
.dn-toggle input:checked + .dn-toggle-track { background: var(--accent); }
.dn-toggle-thumb {
  position: absolute; top: 3px; left: 4px; width: 16px; height: 16px; border-radius: 50%;
  background: var(--text); transition: transform 0.15s ease;
}
.dn-toggle input:checked + .dn-toggle-track .dn-toggle-thumb { transform: translateX(18px); }
.dn-create {
  padding: 11px 22px; border-radius: 10px; font-size: 14px; font-weight: var(--fw-800);
  color: #04140f; background: #34d399; border: none; cursor: pointer;
  transition: transform 0.15s ease, opacity 0.15s ease;
}
.dn-create:hover:not(:disabled) { transform: translateY(-1px); }
.dn-create:disabled { opacity: 0.4; cursor: not-allowed; }
</style>
