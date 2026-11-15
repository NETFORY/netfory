/*
 * Ссылки на обозреватель блоков SmartHoldem (dApp sth://…), новая схема путей:
 *   /tx/<id> · /block/<id> · /wallet/<address>   (старые /#/transaction/… больше не используются)
 */
import { openInMain } from '@/lib/bridge'

export const EXPLORER = 'sth://SXnGWJxC6w5yQZ7LnniEFtuppBfcZkmSN2'

export const explorerTx = (id: string) => `${EXPLORER}/tx/${id}`
export const explorerBlock = (id: string) => `${EXPLORER}/block/${id}`
export const explorerWallet = (address: string) => `${EXPLORER}/wallet/${address}`

export function openExplorerTx(id: string) {
  return openInMain(explorerTx(id)).catch(() => {})
}
export function openExplorerBlock(id: string) {
  return openInMain(explorerBlock(id)).catch(() => {})
}
export function openExplorerWallet(address: string) {
  return openInMain(explorerWallet(address)).catch(() => {})
}
