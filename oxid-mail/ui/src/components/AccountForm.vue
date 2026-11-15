<template>
  <div>
    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2 mb-4">
      <button
        v-for="p in providers"
        :key="p.id"
        type="button"
        class="panel rounded-md p-2.5 text-left transition"
        :class="form.provider === p.id ? 'border-rust' : 'hover:border-white/20'"
        @click="emit('select-provider', p.id)"
        :data-testid="`form-provider-${p.id}`"
      >
        <component
          :is="p.icon"
          class="w-4 h-4 mb-1"
          :class="form.provider === p.id ? 'text-rust' : 'text-iron-100'"
        />
        <div class="text-xs font-semibold">{{ p.label }}</div>
      </button>
    </div>

    <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
      <div class="sm:col-span-2">
        <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">Email</label>
        <input v-model="form.email" type="email" placeholder="you@example.com" class="input-base" data-testid="form-email-input" />
      </div>
      <div class="sm:col-span-2">
        <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">Password / App key</label>
        <input v-model="form.password" type="password" placeholder="•••••••••••" class="input-base" data-testid="form-password-input" />
      </div>
      <div v-if="help" class="sm:col-span-2 panel rounded-md p-3 border border-copper/20" data-testid="form-provider-help">
        <div class="flex items-center gap-2 mb-1.5">
          <KeyRound class="w-3.5 h-3.5 text-copper" />
          <span class="font-mono text-[10px] uppercase tracking-[0.2em] text-copper">{{ help.title }}</span>
        </div>
        <ol class="text-[11px] text-iron-200 list-decimal list-inside space-y-0.5 mb-2">
          <li v-for="s in help.steps" :key="s">{{ s }}</li>
        </ol>
        <button
          type="button"
          class="font-mono text-[11px] text-copper underline decoration-dotted break-all text-left"
          @click="copyHelpUrl"
          title="Click to copy"
          data-testid="form-provider-help-url"
        >
          {{ helpCopied ? '✓ copied to clipboard' : help.url }}
        </button>
      </div>
      <template v-if="form.provider === 'custom'">
        <div>
          <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">IMAP host</label>
          <input v-model="form.host" placeholder="imap.example.com" class="input-base" data-testid="form-imap-host" />
        </div>
        <div>
          <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">IMAP port</label>
          <input v-model="form.port" placeholder="993" class="input-base" data-testid="form-imap-port" />
        </div>
        <div>
          <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">SMTP host</label>
          <input v-model="form.smtpHost" placeholder="smtp.example.com" class="input-base" data-testid="form-smtp-host" />
        </div>
        <div>
          <label class="block text-[10px] uppercase tracking-[0.2em] font-mono text-iron-200 mb-1.5">SMTP port</label>
          <input v-model="form.smtpPort" placeholder="465" class="input-base" data-testid="form-smtp-port" />
        </div>
      </template>
    </div>
  </div>
</template>

<script setup>
import { ref, computed } from 'vue'
import { KeyRound } from 'lucide-vue-next'
import { providerHelp } from '../lib/providerHelp'

const props = defineProps({
  form: { type: Object, required: true },
  providers: { type: Array, required: true },
})
const emit = defineEmits(['select-provider'])

const help = computed(() => providerHelp[props.form.provider] || null)
const helpCopied = ref(false)

function copyHelpUrl() {
  if (!help.value) return
  navigator.clipboard?.writeText(help.value.url).catch(() => {})
  helpCopied.value = true
  window.setTimeout(() => { helpCopied.value = false }, 1500)
}
</script>
