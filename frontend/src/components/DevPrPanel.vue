<script setup lang="ts">
/* eslint-disable no-undef */
// Панель Pull Request'ов репозитория (dev://): входящие для владельца
// (Принять/Отклонить) и создание подписанного PR для контрибьютора.
import { onMounted, ref } from 'vue'
import {
  devhubAcceptPr,
  devhubCreatePr,
  devhubListFiles,
  devhubListPrs,
  devhubReadFile,
  type DevPrFile,
  type DevPullRequest,
} from '@/lib/bridge'
import { useToastsStore } from '@/store/toasts'
import { useDevhubStore } from '@/store/devhub'

const props = defineProps<{ username: string; repo: string; isOwn: boolean; headSha: string }>()
const toasts = useToastsStore()
const devhubStore = useDevhubStore()

const prs = ref<DevPullRequest[]>([])
const loading = ref(true)
const busy = ref<Record<string, boolean>>({})
const expanded = ref<Record<string, boolean>>({})

// ---- форма создания PR (контрибьютор) ----
const showForm = ref(false)
const title = ref('')
const body = ref('')
const files = ref<DevPrFile[]>([])
const newPath = ref('')
const repoFiles = ref<string[]>([])
const saving = ref(false)

async function reload() {
  loading.value = true
  try {
    prs.value = await devhubListPrs(props.username, props.repo)
  } finally {
    loading.value = false
  }
  devhubStore.refreshPrCount()
}

async function accept(pr: DevPullRequest) {
  busy.value[pr.id] = true
  try {
    await devhubAcceptPr(pr.id)
    toasts.push('dev://', `PR «${pr.title}» принят — новый коммит опубликован в сеть`, 'in')
    await reload()
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Не удалось принять PR', 'error')
  } finally {
    busy.value[pr.id] = false
  }
}

async function reject(pr: DevPullRequest) {
  busy.value[pr.id] = true
  try {
    const { devhubRejectPr } = await import('@/lib/bridge')
    await devhubRejectPr(pr.id)
    await reload()
  } finally {
    busy.value[pr.id] = false
  }
}

// ---- редактор файлов PR ----
async function addExistingFile(path: string) {
  if (!path || files.value.some((f) => f.path === path)) return
  try {
    const c = await devhubReadFile(props.username, props.repo, path)
    if (c.binary) {
      toasts.push('dev://', 'Бинарные файлы в PR не поддерживаются', 'error')
      return
    }
    files.value.push({ path, content: c.content, deleted: false })
  } catch {
    toasts.push('dev://', 'Не удалось прочитать файл', 'error')
  }
}

function addNewFile() {
  const p = newPath.value.trim()
  if (!p || files.value.some((f) => f.path === p)) return
  files.value.push({ path: p, content: '', deleted: false })
  newPath.value = ''
}

function removeFile(i: number) {
  files.value.splice(i, 1)
}

async function submit() {
  if (!title.value.trim() || !files.value.length) {
    toasts.push('dev://', 'Нужны заголовок и хотя бы один файл', 'error')
    return
  }
  saving.value = true
  try {
    await devhubCreatePr(props.username, props.repo, title.value, body.value, props.headSha, files.value)
    toasts.push('dev://', 'PR подписан и опубликован — владелец получит его при синке', 'in')
    showForm.value = false
    title.value = ''
    body.value = ''
    files.value = []
    await reload()
  } catch (e: any) {
    toasts.push('dev://', typeof e === 'string' ? e : 'Не удалось создать PR', 'error')
  } finally {
    saving.value = false
  }
}

function fmtDate(secs: number): string {
  return new Date(secs * 1000).toLocaleString()
}

const STATUS_LABEL: Record<string, string> = { open: 'открыт', merged: 'принят', rejected: 'отклонён' }

onMounted(async () => {
  await reload()
  if (!props.isOwn) {
    try {
      repoFiles.value = (await devhubListFiles(props.username, props.repo, ''))
        .filter((f) => !f.isDir)
        .map((f) => f.path)
    } catch { /* файлы недоступны — можно добавить новые */ }
  }
})
</script>

<template>
  <div class="prp" data-testid="dev-pr-panel">
    <div class="prp-head">
      <p class="prp-lead dim">
        {{ isOwn
          ? 'Входящие Pull Request — подписанные патчи от других участников сети. Принятие публикует новый коммит.'
          : 'Ваши Pull Request в этот репозиторий. Патч подписывается ключом кошелька и доезжает до автора по P2P.' }}
      </p>
      <button
        v-if="!isOwn"
        class="prp-new"
        @click="showForm = !showForm"
        data-testid="dev-pr-new-btn"
      >{{ showForm ? '✕ Отмена' : '+ Создать Pull Request' }}</button>
    </div>

    <!-- форма создания -->
    <div v-if="showForm" class="prp-form" data-testid="dev-pr-form">
      <input v-model="title" class="prp-inp" maxlength="140" placeholder="Заголовок PR" data-testid="dev-pr-title-input" />
      <textarea v-model="body" class="prp-inp prp-body" rows="3" placeholder="Описание изменений (опционально)" data-testid="dev-pr-body-input"></textarea>

      <div class="prp-files-edit">
        <div class="prp-add-row">
          <select
            v-if="repoFiles.length"
            class="prp-inp prp-sel"
            @change="addExistingFile(($event.target as HTMLSelectElement).value); ($event.target as HTMLSelectElement).value = ''"
            data-testid="dev-pr-file-select"
          >
            <option value="">— изменить существующий файл —</option>
            <option v-for="p in repoFiles" :key="p" :value="p">{{ p }}</option>
          </select>
          <input v-model="newPath" class="prp-inp" placeholder="или путь нового файла (src/lib.rs)" @keyup.enter="addNewFile" data-testid="dev-pr-newpath-input" />
          <button class="prp-mini" @click="addNewFile" data-testid="dev-pr-addfile-btn">+ файл</button>
        </div>

        <div v-for="(f, i) in files" :key="f.path" class="prp-file-edit">
          <div class="prp-file-head">
            <span class="mono">{{ f.path }}</span>
            <label class="prp-del-cb"><input type="checkbox" v-model="f.deleted" /> удалить файл</label>
            <button class="prp-mini danger" @click="removeFile(i)">✕</button>
          </div>
          <textarea v-if="!f.deleted" v-model="f.content" class="prp-inp prp-code mono" rows="10" :data-testid="`dev-pr-content-${i}`"></textarea>
        </div>
      </div>

      <button class="prp-submit" :disabled="saving" @click="submit" data-testid="dev-pr-submit-btn">
        {{ saving ? 'Подписываем и публикуем…' : '⚿ Подписать и отправить PR' }}
      </button>
    </div>

    <!-- список PR -->
    <p v-if="loading" class="dim prp-empty">Загрузка…</p>
    <p v-else-if="!prs.length" class="dim prp-empty" data-testid="dev-pr-empty">
      {{ isOwn ? 'Входящих Pull Request пока нет.' : 'Вы ещё не отправляли PR в этот репозиторий.' }}
    </p>

    <div v-for="pr in prs" :key="pr.id" class="prp-item" :data-testid="`dev-pr-item-${pr.id}`">
      <div class="prp-item-head" @click="expanded[pr.id] = !expanded[pr.id]">
        <span class="prp-status" :class="`s-${pr.status || 'open'}`">{{ STATUS_LABEL[pr.status] || 'открыт' }}</span>
        <span class="prp-title">{{ pr.title }}</span>
        <span class="dim prp-meta">@{{ pr.author }} · {{ fmtDate(pr.created) }} · {{ pr.files.length }} файл(ов)</span>
      </div>

      <div v-if="expanded[pr.id]" class="prp-detail">
        <p v-if="pr.body" class="prp-desc">{{ pr.body }}</p>
        <p v-if="pr.baseSha" class="dim mono prp-base">base: {{ pr.baseSha.slice(0, 7) }}</p>
        <div v-for="f in pr.files" :key="f.path" class="prp-file-view">
          <div class="prp-file-head">
            <span class="mono">{{ f.path }}</span>
            <span v-if="f.deleted" class="prp-deleted">удаление</span>
          </div>
          <pre v-if="!f.deleted" class="prp-pre mono">{{ f.content }}</pre>
        </div>
        <div v-if="isOwn && (pr.status === 'open' || !pr.status)" class="prp-actions">
          <button class="prp-accept" :disabled="busy[pr.id]" @click="accept(pr)" :data-testid="`dev-pr-accept-${pr.id}`">
            {{ busy[pr.id] ? '…' : '✓ Принять и закоммитить' }}
          </button>
          <button class="prp-rejectb" :disabled="busy[pr.id]" @click="reject(pr)" :data-testid="`dev-pr-reject-${pr.id}`">
            ✕ Отклонить
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.prp { display: flex; flex-direction: column; gap: 12px; }
.prp-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; flex-wrap: wrap; }
.prp-lead { font-size: 12px; max-width: 620px; }
.prp-new {
  padding: 8px 14px; border-radius: 9px; font-size: 12px; font-weight: var(--fw-700); white-space: nowrap;
  color: var(--accent); background: transparent; border: 1px solid var(--border-accent); cursor: pointer;
}
.prp-new:hover { background: rgba(255, 255, 255, 0.04); }
.prp-empty { font-size: 12px; padding: 10px 2px; }
.prp-form { display: flex; flex-direction: column; gap: 8px; padding: 14px; border: 1px solid var(--border-accent); border-radius: 12px; }
.prp-inp {
  width: 100%; padding: 8px 12px; font-size: 12px; color: inherit; border-radius: 8px;
  background: rgba(255, 255, 255, 0.03); border: 1px solid var(--border); outline: none;
}
.prp-inp:focus { border-color: var(--border-accent); }
.prp-body { resize: vertical; }
.prp-code { resize: vertical; font-size: 11.5px; line-height: 1.5; white-space: pre; }
.prp-add-row { display: flex; gap: 8px; align-items: center; }
.prp-sel { max-width: 280px; }
.prp-mini { padding: 7px 10px; font-size: 11px; border-radius: 7px; background: transparent; border: 1px solid var(--border); color: inherit; cursor: pointer; white-space: nowrap; }
.prp-mini.danger { color: var(--danger, #e5534b); }
.prp-files-edit { display: flex; flex-direction: column; gap: 8px; }
.prp-file-edit { display: flex; flex-direction: column; gap: 6px; }
.prp-file-head { display: flex; align-items: center; gap: 12px; font-size: 12px; }
.prp-del-cb { font-size: 11px; color: var(--secondary); display: flex; gap: 5px; align-items: center; }
.prp-submit {
  align-self: flex-start; padding: 9px 18px; font-size: 12px; font-weight: var(--fw-700); border-radius: 9px;
  color: #04110c; background: var(--accent); border: none; cursor: pointer;
}
.prp-submit:disabled { opacity: 0.6; cursor: default; }
.prp-item { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
.prp-item-head { display: flex; align-items: center; gap: 10px; padding: 10px 14px; cursor: pointer; flex-wrap: wrap; }
.prp-item-head:hover { background: rgba(255, 255, 255, 0.02); }
.prp-status { font-size: 10px; font-weight: var(--fw-700); text-transform: uppercase; padding: 2px 8px; border-radius: 20px; }
.prp-status.s-open { color: var(--accent); border: 1px solid var(--border-accent); }
.prp-status.s-merged { color: #9a6ff0; border: 1px solid #9a6ff055; }
.prp-status.s-rejected { color: var(--danger, #e5534b); border: 1px solid rgba(229, 83, 75, 0.4); }
.prp-title { font-size: 13px; font-weight: var(--fw-700); }
.prp-meta { font-size: 11px; }
.prp-detail { padding: 4px 14px 14px; display: flex; flex-direction: column; gap: 8px; border-top: 1px solid var(--border); }
.prp-desc { font-size: 12px; padding-top: 8px; }
.prp-base { font-size: 11px; }
.prp-file-view { border: 1px solid var(--border); border-radius: 8px; padding: 8px 10px; }
.prp-deleted { font-size: 10px; color: var(--danger, #e5534b); text-transform: uppercase; }
.prp-pre { margin: 6px 0 0; padding: 8px; font-size: 11px; line-height: 1.5; max-height: 320px; overflow: auto; background: rgba(0, 0, 0, 0.25); border-radius: 6px; }
.prp-actions { display: flex; gap: 10px; }
.prp-accept { padding: 8px 16px; font-size: 12px; font-weight: var(--fw-700); border-radius: 8px; color: #04110c; background: var(--accent); border: none; cursor: pointer; }
.prp-rejectb { padding: 8px 16px; font-size: 12px; border-radius: 8px; color: var(--danger, #e5534b); background: transparent; border: 1px solid rgba(229, 83, 75, 0.4); cursor: pointer; }
</style>
