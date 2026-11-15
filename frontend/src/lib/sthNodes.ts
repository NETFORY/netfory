/*
 * Встроенный пул P2P-узлов SmartHoldem (api://<EndpointId>/sth-core) — зеркало
 * DEFAULT_IROH_NODES в src-tauri/src/sth_node.rs. Env SMARTHOLDEM_NODES_P2P
 * дополняет список (дедуп по адресу), а не заменяет его.
 */
export const DEFAULT_P2P_NODES: readonly string[] = [
  'api://1692c5f342542465819ff31c31432e933455ef3daa788d7297065869e723b790/sth-core',
  'api://d8c32e15974a4197f16d61b7cc03e4be4338cde2deca6310589584adbc465bf6/sth-core',
  'api://b429afd5ce4f59989ab0f87d2e4778cfaac94ed27e2ffd5c32998661ed99f761/sth-core',
  'api://d3e9d5fd1904cc5c1189c1fa3de219b94fe8dc1bcca43b5c81590384a3e352b2/sth-core',
  'api://58513d44a5ccbd8605633a1e7568bda0caca930ae02f73dd21b6eb0dd4bca76c/sth-core',
  'api://74e98fd504bec4337d958f55aa72cd07a8962128508dc63b6e797a82295c3520/sth-core',
  'api://67ea4f941b41e06d51140586d9d222ec3dac88e7a91c5fdc49d2788bec83c8b4/sth-core',
  'api://3d446a8bc525d1060200ba99939515385218744211f412c95703974700850b73/sth-core',
  'api://3852440f15a0ad9527426b4ae0f25889f4744f0302c06b2eb8487f2e561fdcf6/sth-core',
  'api://3e141032f9996c63b7bab2c47c6bd2778a6656de08ca33ff8d9a93ebb091bb7a/sth-core',
] as const

const norm = (s: string) => s.trim().replace(/\/+$/, '')

/** Встроенные + из env (SMARTHOLDEM_NODES_P2P), только api://, без дублей. */
export function builtinP2pNodes(): string[] {
  const env = (((import.meta as any).env?.SMARTHOLDEM_NODES_P2P as string) || '')
    .split(',')
    .map(norm)
    .filter((s) => s.startsWith('api://'))
  return Array.from(new Set([...DEFAULT_P2P_NODES.map(norm), ...env]))
}
