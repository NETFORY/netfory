<script setup lang="ts">
/*
 * SmartNet Wallet // WalletSettingsView
 *
 *   Внутрикошельковые настройки. Сейчас: единственная кнопка "Показать
 *   приватный ключ (mnemonic)". Держится в отдельной панели чтобы не
 *   загромождать Dashboard, и чтобы легко расширять раздел позже
 *   (auto-lock timeout, hide balance, custom RPC endpoint и т.д.).
 *
 *   Reveal-flow — двухшаговый:
 *     1) Пользователь нажимает "Показать" → предупреждение и подтверждение.
 *     2) После confirm — фраза видна ~30с, потом автоматически скрывается.
 *   Ключ не логируется, не отправляется по сети — берётся из in-memory
 *   `SESSION_SEED` (Rust `reveal_seed()`), кошелёк уже разблокирован.
 */
import { onBeforeUnmount, ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useWalletStore } from '@/store/wallet'
import { isTauri, nativeRevealSeed } from '@/lib/bridge'
import { shortAddr } from '@/lib/crypto'

const { t } = useI18n()
const wallet = useWalletStore()
const accents = [
  { id: 'teal', color: '#2ee3c6' },
  { id: 'orange', color: '#ff7a1a' },
  { id: 'violet', color: '#7c6cff' },
] as const

const seed = ref<string | null>(null)
const armed = ref(false) // подтверждение "я понимаю риск"
const revealing = ref(false)
const secondsLeft = ref(0)
let hideTimer: ReturnType<typeof setInterval> | null = null

/** Компактное представление слов (12/24), нумерованный сетчатый макет. */
const words = computed<string[]>(() => {
  const s = (seed.value || '').trim()
  return s ? s.split(/\s+/) : []
})

async function reveal() {
  wallet.error = ''
  revealing.value = true
  try {
    if (isTauri) {
      seed.value = await nativeRevealSeed()
    } else {
      // Web-preview: если в LS есть sth_vault, показываем плейсхолдер (реальный
      // мнемоник в браузере не хранится в открытом виде — только зашифрованный).
      seed.value =
        'abandon ability able about above absent absorb abstract absurd abuse access accident'
    }
    if (!seed.value) {
      wallet.error = t('wallet.settings.notUnlocked')
      return
    }
    // авто-скрытие: 30 секунд с обратным отсчётом
    secondsLeft.value = 30
    hideTimer && clearInterval(hideTimer)
    hideTimer = setInterval(() => {
      secondsLeft.value -= 1
      if (secondsLeft.value <= 0) hide()
    }, 1000)
  } catch (e: any) {
    wallet.error = String(e?.message ?? e ?? 'reveal failed')
  } finally {
    revealing.value = false
  }
}

function hide() {
  seed.value = null
  armed.value = false
  secondsLeft.value = 0
  if (hideTimer) {
    clearInterval(hideTimer)
    hideTimer = null
  }
}

async function copyToClipboard() {
  if (!seed.value) return
  try {
    await navigator.clipboard.writeText(seed.value)
    wallet.flash(t('wallet.settings.copied'))
  } catch {
    wallet.flash(t('wallet.settings.copyFail'))
  }
}

onBeforeUnmount(hide)
</script>

<template>
  <div class="wsettings" data-testid="wallet-settings">
    <p class="hdr-title">{{ t('wallet.settings.title') }}</p>

    <!-- Узлы сети: перенесены из нижней панели в настройки -->
    <button class="nodes-entry" @click="wallet.setView('nodes')" data-testid="settings-nodes-entry">
      <span class="ne-ic">⬢</span>
      <span class="ne-txt"><b>{{ t('wallet.bottom.nodes') }}</b><small class="dim">{{ t('wallet.nodes.title') }}<template v-if="!wallet.streamLive"> · {{ t('wallet.nodes.streamLegacy') }}</template></small></span>
      <span class="ne-chev">›</span>
    </button>

    <!-- Section: Accent (три облика, как в расширении) -->
    <div class="section neutral" data-testid="settings-accent-section">
      <div class="section-hdr">
        <span class="section-title">{{ t('wallet.settings.accentTitle') }}</span>
      </div>
      <p class="section-desc dim">{{ t('wallet.settings.accentDesc') }}</p>
      <div class="accents">
        <button
          v-for="a in accents"
          :key="a.id"
          class="accent-btn"
          :class="{ on: wallet.accent === a.id }"
          :style="{ '--sw': a.color }"
          @click="wallet.setAccent(a.id)"
          :data-testid="`settings-accent-${a.id}`"
        >
          <span class="sw" />
          <span class="accent-lbl">{{ t(`wallet.settings.accent.${a.id}`) }}</span>
        </button>
      </div>
    </div>

    <!-- Section: Reveal Private Key -->
    <div class="section" data-testid="settings-reveal-section">
      <div class="section-hdr">
        <span class="section-title">{{ t('wallet.settings.revealTitle') }}</span>
        <span class="section-badge danger">{{ t('wallet.settings.dangerZone') }}</span>
      </div>
      <p class="section-desc dim">{{ t('wallet.settings.revealDesc') }}</p>

      <!-- Информация об активном адресе -->
      <div class="signer-row">
        <span class="dim">{{ t('wallet.settings.forAddress') }}</span>
        <code class="mono" data-testid="settings-address">{{ shortAddr(wallet.address, 8, 6) }}</code>
      </div>

      <!-- Fase 1: непоказанное состояние — arming checkbox -->
      <div v-if="!seed" class="arm-block">
        <label class="arm-check" data-testid="settings-arm-check">
          <input v-model="armed" type="checkbox" />
          <span class="arm-lbl">{{ t('wallet.settings.iUnderstand') }}</span>
        </label>
        <button
          class="btn btn-danger act"
          :disabled="!armed || revealing"
          @click="reveal"
          data-testid="settings-reveal-btn"
        >
          {{ revealing ? t('wallet.settings.revealing') : t('wallet.settings.revealBtn') }}
        </button>
      </div>

      <!-- Fase 2: показ слов + автоскрытие -->
      <div v-else class="reveal-block" data-testid="settings-reveal-block">
        <div class="countdown">
          <span class="dim">{{ t('wallet.settings.autoHide') }}</span>
          <code class="mono" data-testid="settings-countdown">{{ secondsLeft }}s</code>
        </div>
        <div class="words-grid" data-testid="settings-words-grid">
          <div
            v-for="(w, i) in words"
            :key="i"
            class="word-cell"
            :data-testid="`settings-word-${i + 1}`"
          >
            <span class="word-idx">{{ i + 1 }}</span>
            <span class="word-txt mono">{{ w }}</span>
          </div>
        </div>
        <div class="reveal-acts">
          <button class="btn" @click="copyToClipboard" data-testid="settings-copy-btn">
            ⧉ {{ t('wallet.settings.copy') }}
          </button>
          <button class="btn btn-primary" @click="hide" data-testid="settings-hide-btn">
            ✕ {{ t('wallet.settings.hide') }}
          </button>
        </div>
      </div>

      <p v-if="wallet.error" class="err" data-testid="settings-error">⚠ {{ wallet.error }}</p>
    </div>
  </div>
</template>

<style scoped>
.nodes-entry { display: flex; align-items: center; gap: 12px; width: 100%; padding: 13px 14px; margin-bottom: 14px; border-radius: 10px; border: 1px solid var(--border); background: var(--surface, transparent); color: var(--text); font: inherit; text-align: left; cursor: pointer; transition: border-color 0.15s; }
.nodes-entry:hover { border-color: var(--accent); }
.ne-ic { color: var(--accent); font-size: 16px; }
.ne-txt { flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.ne-txt b { font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; }
.ne-txt small { font-size: 11px; }
.ne-chev { color: var(--text-dim); font-size: 18px; }
.wsettings {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.hdr-title {
  margin: 0;
  font-size: 11px;
  font-weight: var(--fw-800);
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--accent);
}
.section {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px;
  border: 1px solid color-mix(in srgb, var(--danger) 40%, var(--border));
  border-radius: var(--radius);
  background: var(--panel);
}
.section.neutral {
  border-color: var(--border);
}
.accents {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
}
.accent-btn {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 12px 6px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg-elevated);
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color 0.15s ease, transform 0.15s ease, color 0.15s ease;
}
.accent-btn:hover {
  transform: translateY(-1px);
  border-color: var(--sw);
}
.accent-btn.on {
  border-color: var(--sw);
  color: var(--text);
  box-shadow: 0 0 0 1px var(--sw), 0 0 18px -8px var(--sw);
}
.sw {
  width: 26px;
  height: 26px;
  border-radius: 50%;
  background: var(--sw);
  box-shadow: 0 0 12px -2px var(--sw);
}
.accent-btn.on .sw {
  outline: 2px solid var(--panel);
  outline-offset: -4px;
}
.accent-lbl {
  font-size: 9px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  font-weight: var(--fw-800);
}
.section-hdr {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.section-title {
  font-size: 12px;
  font-weight: var(--fw-800);
  color: var(--text);
  letter-spacing: 0.06em;
}
.section-badge {
  font-size: 9px;
  font-weight: var(--fw-800);
  letter-spacing: 0.14em;
  padding: 3px 8px;
  border-radius: 999px;
}
.section-badge.danger {
  color: var(--danger);
  background: color-mix(in srgb, var(--danger) 18%, transparent);
  border: 1px solid color-mix(in srgb, var(--danger) 45%, transparent);
}
.section-desc {
  font-size: 11.5px;
  line-height: 1.55;
  margin: 0;
}
.signer-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: color-mix(in srgb, var(--bg) 60%, transparent);
  font-size: 11.5px;
}
.arm-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.arm-check {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11.5px;
  cursor: pointer;
  color: var(--text-dim);
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
}
.arm-check:hover {
  color: var(--text);
  border-color: var(--border-accent);
}
.arm-lbl {
  flex: 1;
  line-height: 1.4;
}
.act {
  padding: 12px;
  font-weight: var(--fw-700);
  font-size: 11.5px;
  letter-spacing: 0.08em;
}
.reveal-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.countdown {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}
.countdown code {
  color: var(--accent);
  font-weight: var(--fw-800);
  font-size: 12px;
}
.words-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
  padding: 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: color-mix(in srgb, var(--bg) 70%, transparent);
}
.word-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 4px;
}
.word-idx {
  font-size: 9px;
  font-weight: var(--fw-800);
  color: var(--text-dim);
  min-width: 14px;
  text-align: right;
}
.word-txt {
  font-size: 12px;
  color: var(--accent);
  font-weight: var(--fw-700);
}
.reveal-acts {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 8px;
}
.reveal-acts .btn {
  padding: 10px;
  font-size: 11px;
  font-weight: var(--fw-700);
  letter-spacing: 0.06em;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.err {
  color: var(--danger);
  font-size: 11.5px;
  margin: 0;
}
</style>
