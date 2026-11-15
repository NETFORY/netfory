import { defineStore } from 'pinia'

/**
 * Cчётчик успешных фоновых репликаций Cloud Seeder за ТЕКУЩУЮ сессию.
 * Каждое событие `seeder-replicated` (см. AppLayout) увеличивает счётчик;
 * `startSeederReplication` вызывает первый tick сразу после разблокировки,
 * поэтому badge появляется через ~5-10 секунд, если облако доступно.
 *
 * Хранить между сессиями смысла нет — идея бейджа в том, чтобы визуально
 * показать «сидер жив прямо сейчас», а не суммарную историю за всё время.
 */
export const useSeedersStatsStore = defineStore('seedersStats', {
  state: () => ({
    /** Количество успешных фоновых репликаций с момента разблокировки. */
    sessionReplications: 0,
    /** Timestamp (ms) последней репликации — для tooltip'а в сайдбаре. */
    lastReplicationAt: 0,
  }),
  actions: {
    tickReplicated() {
      this.sessionReplications += 1
      this.lastReplicationAt = Date.now()
    },
    /** Сброс при lock() — новая сессия должна стартовать с чистого счётчика. */
    reset() {
      this.sessionReplications = 0
      this.lastReplicationAt = 0
    },
  },
})
