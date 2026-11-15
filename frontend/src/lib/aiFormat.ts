/*
 * aiFormat.ts — универсальное форматирование ёмкостей/мощностей AI Pool.
 */

/** МБ → строка в ГБ с одним знаком (24576 → "24.0"). */
export function gbFromMb(mb: number): string {
  return (mb / 1024).toFixed(1)
}

/** ГБ → "X GB" | "X TB" при переходе за терабайт. */
export function fmtCapacityGb(gb: number): string {
  if (gb >= 1024) return `${(gb / 1024).toFixed(2)} TB`
  return `${gb.toFixed(1)} GB`
}

/** TFLOPS → "X TFLOPS" | "X PFLOPS" при переходе за петафлопс. */
export function fmtCompute(tflops: number): string {
  if (tflops >= 1000) return `${(tflops / 1000).toFixed(2)} PFLOPS`
  return `${tflops.toFixed(1)} TFLOPS`
}
