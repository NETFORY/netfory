<script setup lang="ts">
/*
 * Скелетон дашборда кошелька — показывается ТОЛЬКО при самом первом открытии,
 * когда в localStorage ещё нет снимка (аккаунт, баланс, история). Повторяет
 * геометрию DashboardView: чип аккаунта → карточка баланса → 4 действия →
 * 3 строки активности. Дальше панель всегда рендерится из снимка мгновенно.
 */
</script>

<template>
  <div class="wsk" data-testid="wallet-skeleton" aria-busy="true">
    <div class="wsk-acc">
      <span class="sk sk-idx" />
      <div class="wsk-acc-txt">
        <span class="sk sk-line w60" />
        <span class="sk sk-line w120 thin" />
      </div>
      <span class="sk sk-ico" />
      <span class="sk sk-ico" />
    </div>

    <div class="wsk-bal">
      <div class="wsk-bal-head">
        <span class="sk sk-line w90 thin" />
        <span class="sk sk-line w50 thin" />
      </div>
      <span class="sk sk-big" />
      <div class="wsk-bal-sub">
        <span class="sk sk-line w80 thin" />
        <span class="sk sk-line w110 thin" />
      </div>
      <span class="sk sk-spark" />
    </div>

    <div class="wsk-actions">
      <span v-for="i in 4" :key="i" class="sk sk-act" />
    </div>

    <div class="wsk-act-head">
      <span class="sk sk-line w100 thin" />
      <span class="sk sk-line w60 thin" />
    </div>
    <div v-for="i in 3" :key="i" class="wsk-row">
      <span class="sk sk-ico lg" />
      <div class="wsk-row-txt">
        <span class="sk sk-line w70" />
        <span class="sk sk-line w100 thin" />
      </div>
      <div class="wsk-row-amt">
        <span class="sk sk-line w90" />
        <span class="sk sk-line w40 thin" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.wsk {
  display: flex;
  flex-direction: column;
  gap: 12px;
  animation: wsk-in 0.25s ease-out;
}
@keyframes wsk-in {
  from { opacity: 0; transform: translateY(4px); }
  to { opacity: 1; transform: none; }
}
.sk {
  display: block;
  border-radius: 6px;
  background: linear-gradient(
    90deg,
    rgba(255, 255, 255, 0.05) 0%,
    rgba(255, 255, 255, 0.11) 45%,
    rgba(255, 255, 255, 0.05) 90%
  );
  background-size: 220% 100%;
  animation: wsk-shimmer 1.4s ease-in-out infinite;
}
@keyframes wsk-shimmer {
  0% { background-position: 120% 0; }
  100% { background-position: -120% 0; }
}
.sk-line { height: 11px; }
.sk-line.thin { height: 8px; opacity: 0.75; }
.w40 { width: 40px; }
.w50 { width: 50px; }
.w60 { width: 60px; }
.w70 { width: 70px; }
.w80 { width: 80px; }
.w90 { width: 90px; }
.w100 { width: 100px; }
.w110 { width: 110px; }
.w120 { width: 120px; }
.sk-idx { width: 28px; height: 28px; border-radius: 8px; }
.sk-ico { width: 30px; height: 30px; border-radius: 9px; }
.sk-ico.lg { width: 36px; height: 36px; }

.wsk-acc {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 14px;
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.05);
}
.wsk-acc-txt { flex: 1; display: flex; flex-direction: column; gap: 6px; }

.wsk-bal {
  padding: 16px 16px 12px;
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.05);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.wsk-bal-head, .wsk-bal-sub, .wsk-act-head { display: flex; justify-content: space-between; align-items: center; }
.sk-big { width: 62%; height: 34px; border-radius: 8px; }
.sk-spark { width: 100%; height: 44px; border-radius: 8px; opacity: 0.6; }

.wsk-actions { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; }
.sk-act { height: 68px; border-radius: 14px; }

.wsk-act-head { margin-top: 6px; padding: 0 2px; }
.wsk-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.05);
}
.wsk-row-txt { flex: 1; display: flex; flex-direction: column; gap: 6px; }
.wsk-row-amt { display: flex; flex-direction: column; align-items: flex-end; gap: 6px; }
</style>
