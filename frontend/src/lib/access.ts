/**
 * File Exchange page visibility gate.
 *
 * `FILE_EXCHANGE_PAGE_ALLOW` (frontend/.env, comma-separated SmartHoldem
 * addresses):
 *   - UNSET / empty  → everyone sees the "File Exchange" tab (default).
 *   - set            → ONLY the listed addresses see the global "File Exchange"
 *                      browse tab; everyone else gets just "My Files" (their own
 *                      files + stats; they still receive files via shared links).
 */
export function fileExchangeAllowList(): string[] {
  const raw = ((import.meta as any).env?.FILE_EXCHANGE_PAGE_ALLOW as string) || ''
  return raw
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
}

export function canSeeFileExchange(address: string): boolean {
  const list = fileExchangeAllowList()
  if (list.length === 0) return true
  return list.includes(address)
}
