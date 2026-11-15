// Rolling history of the Mainline DHT routing-table size, persisted in
// localStorage so the mini-sparkline next to the counter survives navigation
// and app restarts. Shared by Dashboard + Network Map (single source).
const KEY = 'sth_dht_hist'
const MAX = 48

export function getDhtHistory(): number[] {
  try {
    const raw = localStorage.getItem(KEY)
    if (raw) {
      const a = JSON.parse(raw)
      if (Array.isArray(a)) return a.filter((x) => typeof x === 'number' && x >= 0)
    }
  } catch { /* ignore */ }
  return []
}

export function pushDht(n: number): number[] {
  if (!(n > 0)) return getDhtHistory()
  const arr = getDhtHistory()
  // Skip consecutive duplicates so the line reflects real movement.
  if (arr.length && arr[arr.length - 1] === n) return arr
  arr.push(n)
  while (arr.length > MAX) arr.shift()
  try { localStorage.setItem(KEY, JSON.stringify(arr)) } catch { /* ignore */ }
  return arr
}
