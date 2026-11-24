<script setup lang="ts">
import { computed, defineExpose, defineProps } from 'vue'
import { useWalletStore } from '@/store/wallet'
import { fromUnits, toUnits } from './api'
import { useTokT } from './t'

// Итог перед подтверждением: строки операции + комиссия сети + баланс STH после комиссии.
const props = defineProps<{ rows: [string, string][]; fee: string; spend?: bigint }>()
const tt = useTokT()
const wallet = useWalletStore()
const after = computed(() => {
  const b = toUnits(String(wallet.balance).replace(/[\s,]/g, ''), 8) ?? 0n
  const r = b - BigInt(props.fee || '0') - (props.spend ?? 0n)
  return { text: fromUnits(r < 0n ? 0n : r, 8), low: r < 0n }
})
defineExpose({ low: computed(() => after.value.low) })
</script>

<template>
  <dl class="tsum" data-testid="token-summary">
    <div v-for="[k, v] in rows" :key="k"><dt>{{ k }}</dt><dd>{{ v }}</dd></div>
    <hr />
    <div class="fee"><dt>{{ tt('netFee') }}</dt><dd>{{ fromUnits(fee, 8) }} STH</dd></div>
    <div :class="{ low: after.low }"><dt>{{ tt('balAfter') }}</dt><dd data-testid="token-summary-after">{{ after.low ? tt('errSth') : `${after.text} STH` }}</dd></div>
  </dl>
</template>

<style scoped>
.tsum { margin: 0; padding: 12px 14px; border-radius: 12px; border: 1px solid var(--border); background: color-mix(in srgb, var(--bg) 60%, transparent); display: flex; flex-direction: column; gap: 9px; }
.tsum div { display: flex; justify-content: space-between; gap: 12px; }
.tsum dt { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-dim); }
.tsum dd { margin: 0; font-size: 12.5px; font-family: var(--font-mono); text-align: right; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tsum hr { border: none; border-top: 1px solid var(--border); margin: 2px 0; }
.tsum .fee dt, .tsum .fee dd { color: var(--accent); font-weight: 700; }
.tsum .low dd { color: #f87171; }
</style>
