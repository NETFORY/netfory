// Мок Tauri IPC для визуальной проверки нативных вкладок Settings в браузере (Playwright add_init_script).

// Моки страны выхода pq-web (SitePanel): узлы, закреп страны по сайту, ротация «Новой личности»
const fkExit = {
  nodes: [
    { id: 'pqde1'.padEnd(52, 'a'), iso: 'DE', addr: '88.198.12.7:7843', region: 'de-fra' },
    { id: 'pqde2'.padEnd(52, 'b'), iso: 'DE', addr: '136.243.10.5:7843', region: 'de' },
    { id: 'pqnl1'.padEnd(52, 'c'), iso: 'NL', addr: '51.15.84.20:7843', region: 'nl-ams' },
    { id: 'pqfi1'.padEnd(52, 'd'), iso: 'FI', addr: '95.216.33.1:7843', region: 'de', real: 'SE', realIp: '185.70.42.9' },
  ],
  pins: {}, cur: {}, fb: {}, def: '', ver: {},
  key: (h) => String(h || '').split('.').slice(-2).join('.'),
  iso(n) { return this.ver[n.id] || n.iso },
  eff(k) { const p = this.pins[k]; return p === '*' ? '' : p || this.def },
  pool(k) { const p = this.eff(k); const f = p ? this.nodes.filter((n) => this.iso(n) === p) : []; this.fb[k] = !!p && !f.length; return f.length ? f : this.nodes },
  view(h) {
    const k = this.key(h), pool = this.pool(k)
    if (!pool.includes(this.cur[k])) this.cur[k] = pool[0]
    const c = this.cur[k], cc = {}
    this.nodes.forEach((n) => { cc[this.iso(n)] = (cc[this.iso(n)] || 0) + 1 })
    const reg = c.region.slice(0, 2).toUpperCase()
    return { key: k, direct: !!window.__fkDirect, pinned: this.pins[k] || '', default: this.def, effective: this.eff(k), fallback: this.fb[k], current: h ? { id: c.id, addr: c.addr, iso: this.iso(c), region: c.region, verified: !!this.ver[c.id], mismatch: reg !== this.iso(c) } : null, countries: Object.entries(cc).sort().map(([iso, nodes]) => ({ iso, nodes })) }
  },
  pin(h, iso) { const k = this.key(h); if (iso) this.pins[k] = String(iso).toUpperCase(); else delete this.pins[k]; delete this.cur[k]; return null },
  setDef(iso) { this.def = String(iso || '').toUpperCase(); return null },
  check(h) { const v = this.view(h), n = this.cur[this.key(h)], exp = this.iso(n); const real = n.real || n.iso; const ip = n.realIp || n.addr.replace(/:\d+$/, ''); this.ver[n.id] = real; return { ip, iso: real, city: 'Mock City', org: 'Mock ASN', service: 'ifconfig.co', node: n.id, expected: exp, announced: [n.addr.replace(/:\d+$/, '')], ipMatch: !n.realIp, countryMatch: real === exp, _v: v.key } },
  rotate(h) { const k = this.key(h), pool = this.pool(k), prev = this.cur[k] || pool[0]; this.cur[k] = pool[(pool.indexOf(prev) + 1) % pool.length]; return prev.id },
};
(() => {
  const acc = { index: 0, path: "m/44'/63'/0'/0/0", address: 'SF8tVhzQ4mZk9hwm', publicKey: '02ab'.padEnd(66, 'c') }
  const pq = {
    enabled: true, port: 52753, total: false,
    bundles: [
      { name: 'iptest', hosts: ['2ip.ru', '*.2ip.ru', '2ip.io'], mode: 'relay', route: 'relay', reason: 'mode' },
      { name: 'telegram', hosts: ['telegram.org', '*.telegram.org', 't.me'], mode: 'auto', route: null, reason: '' },
      { name: 'x', hosts: ['x.com', '*.x.com', 'twitter.com'], mode: 'auto', route: 'direct', reason: 'ok' },
      { name: 'youtube', hosts: ['youtube.com', '*.youtube.com', 'youtu.be'], mode: 'auto', route: 'relay', reason: 'tls-stall' },
    ],
    nodes: [
      { id: '8ed2c3e8a1b2c3d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e237d', region: 'NL', source: 'tracker', verified: true, addr: '', open: true, ipv6: true, ok: true, rttMs: 72, checkedAt: 0, error: '' },
      { id: 'z9x7v5t3r1p8n6l4j2h9f7d5b3a1c8e6g4i2k9m7o5q3s1u8w6y4a2q1w2e3r4t5', region: 'DE', source: 'manual', verified: false, addr: '', open: false, ipv6: false, ok: false, rttMs: 0, checkedAt: 0, error: 'connect timeout' },
    ],
    active: {},
    manualNodes: [{ id: 'z9x7v5t3r1p8n6l4j2h9f7d5b3a1c8e6g4i2k9m7o5q3s1u8w6y4a2q1w2e3r4t5', region: 'DE' }],
    auto: { nodes: [{ id: '8ed2c3e8a1b2c3d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e237d', region: 'NL', verified: true, ver: '0.2.0' }], signed: false, tracker: '', at: Math.floor(Date.now() / 1000) - 1680, stale: false, error: 'трекер #3: узлов pq-web пока нет' },
  }
  const myc = {
    // настройки переживают перезагрузку страницы (как sled + mycelium.json в Tauri)
    s: (() => { try { return { enabled: false, allowDirect: false, voice: false, clientOnly: false, bandwidthKb: 2048, hops: 3, bootstrap: [], coverTraffic: false, ...JSON.parse(localStorage.getItem('fk_mycelium') || '{}') } } catch { return { enabled: false, allowDirect: false, voice: false, clientOnly: false, bandwidthKb: 2048, hops: 3, bootstrap: [], coverTraffic: false } } })(),
    at: 0, dvpn: false, gen: 0, rebuiltAt: 0,
    // вторая цепочка деградировала (Middle молчит, авто-попытки 3/3) — пока не нажата «Построить новую цепочку»
    eff() {
      const b = this.s.bootstrap.length
      return b > 0 && b < this.s.hops ? b : this.s.hops
    },
    circuits(n) {
      const base = [38, 52, 61], hops = this.eff()
      return Array.from({ length: n }, (_, i) => {
        let acc = 0
        const rtt = base.slice(0, hops).map((b) => (acc += Math.round(b * (0.8 + Math.random() * 0.5) + i * 7)))
        const alive = Array.from({ length: hops }, () => true)
        const degraded = i === 1 && hops > 1 && this.rebuiltAt === 0
        if (degraded) { alive[1] = false; for (let k = 1; k < rtt.length; k++) rtt[k] += 940 }
        const rebuilding = i === 1 && this.rebuiltAt > 0 && Date.now() - this.rebuiltAt < 1800
        return { id: 0x80000000 + i * 977 + (i === 1 ? this.gen * 13 : 0), path: Array.from({ length: hops }, (_, h) => `${'abcdef'[(i + h + (i === 1 ? this.gen : 0)) % 6]}${i}${h}`.padEnd(64, String((i + h) % 10))), hopRttMs: rtt, hopAlive: alive, target: i === 0 ? '9d41'.padEnd(64, '7') : i === 1 ? 'e7a2'.padEnd(64, '5') : null, e2eMs: i === 0 ? rtt[rtt.length - 1] + 80 : null, e2eAgeSecs: i === 0 ? 12 : null, ageSecs: 140 + i * 60, cellsIn: 120 + i, cellsOut: 98 + i, retries: degraded ? 3 : 0, rebuilding, degraded, kind: i === 0 ? 'rendezvous' : i === 2 ? 'intro' : 'exit' }
      })
    },
    async rebuild() {
      this.rebuiltAt = Date.now()
      await new Promise((r) => setTimeout(r, 1500))
      this.gen += 1
      return this.circuits(2)[1]
    },
    info() {
      const age = Date.now() - this.at, on = this.s.enabled
      const ready = on ? (age > 2500 ? 3 : age > 1200 ? 1 : 0) : 0
      return {
        circuits: ready ? this.circuits(ready) : [],
        settings: { ...this.s }, started: true, running: on, transportId: on ? '7c1e'.padEnd(64, 'b') : '', mobile: false, nodeId: '5f3a'.padEnd(64, 'e'), systemDaemon: false, socksPort: 9150, dvpnPaused: this.dvpn, vpn: this.vpn(),
        status: { enabled: on, circuitsReady: ready, circuitsBuilding: on && ready < 3, relaysKnown: on ? 7 : 0, guards: on ? ['a1b2c3d4'.padEnd(64, '0'), 'b2c3d4e5'.padEnd(64, '1'), 'c3d4e5f6'.padEnd(64, '2')] : [], hops: this.s.hops, relayEnabled: on && !this.s.clientOnly, relayCircuitsActive: on ? 2 : 0, relayCircuitsTotal: on ? 14 : 0, relayCells: on ? 48213 : 0, relayBytes: on ? 48213 * 2048 : 0, exitStreams: on ? 31 : 0, lastError: '', hidden: true, introReady: on && age > 2000 ? 2 : 0, servicesKnown: on ? 4 : 0, coverTraffic: this.s.coverTraffic, relayIntroActive: on ? 1 : 0, relayRendezvousTotal: on ? 6 : 0, relaysUsable: on ? (this.eff() < this.s.hops ? this.eff() : 7) : 0, hopsEffective: ready ? this.eff() : 0, hopsReduced: !!ready && this.eff() < this.s.hops },
      }
    },
    set(n) {
      if (n.enabled && !this.s.enabled) { this.at = Date.now(); if (pq.total) { this.dvpn = true; pq.total = false } }
      if (!n.enabled && this.s.enabled && this.dvpn) { this.dvpn = false; pq.total = true }
      if (!n.enabled && this.s.enabled) this.vpnAt = 0
      this.s = { ...n, voice: false }
      try { localStorage.setItem('fk_mycelium', JSON.stringify(this.s)) } catch { /* noop */ }
      return this.info()
    },
    // Фаза 3: системный VPN — в моке туннель «поднимается» через 3 с после включения
    vpnAt: 0,
    vpn() {
      const on = this.vpnAt > 0, up = on ? Math.floor((Date.now() - this.vpnAt) / 1000) : 0, running = on && up >= 3
      const secs = running ? up - 3 : 0
      return { enabled: on, running, error: '', supported: true, helperFound: true, socksPort: on ? 41873 : 0, bypass: on ? 5 : 0, tx: secs * 18400, rx: secs * 71200, upSecs: secs, sessions: running ? 4 + Math.floor(secs / 2) : 0 }
    },
    vpnSet(on) {
      if (on && !this.s.enabled) throw new Error('turn Mycelium on first')
      if (on && this.info().status.circuitsReady === 0) throw new Error('no circuits yet — wait until Mycelium is active')
      this.vpnAt = on ? Date.now() : 0
      return this.info()
    },
  }
  const table = {
    vault_exists: () => !!localStorage.getItem('fk_vault'),
    load_vault: () => ({ address: acc.address, publicKey: acc.publicKey, accounts: [0], createdAt: new Date().toISOString() }),
    create_vault: () => { localStorage.setItem('fk_vault', '1'); return acc },
    unlock_vault: () => acc,
    webproxy_status: () => JSON.parse(JSON.stringify(pq)),
    webproxy_set_total: (a) => { pq.total = !!a.on; return null },
    mycelium_status: () => myc.info(),
    p2p_status: () => ({ enabled: true, nodeId: '5f3a'.padEnd(64, 'e') }),
    mycelium_set: (a) => myc.set(a.settings),
    mycelium_circuit: () => (myc.s.enabled ? myc.circuits(1)[0] : null),
    mycelium_rebuild: () => myc.rebuild(),
    mycelium_vpn_set: (a) => myc.vpnSet(!!a.on),
    autostart_get: () => { try { return { closeToTray: true, launchAtLogin: false, ...JSON.parse(localStorage.getItem('fk_autostart') || '{}') } } catch { return { closeToTray: true, launchAtLogin: false } } },
    autostart_set: (a) => { try { localStorage.setItem('fk_autostart', JSON.stringify(a.settings)) } catch { /* noop */ } return { ...a.settings } },
    open_link_in_main: (a) => { window.__lastOpenLink = a.url; window.dispatchEvent(new CustomEvent('netfory:open-link', { detail: a.url })); return null },
    open_external: (a) => { window.__lastExternal = a.url; return null },
    webproxy_site_bytes: (a) => { const k = String(a.host || '').split('.').slice(-2).join('.'); const m = (window.__fkSite ||= {})[k] ||= { key: k, rx: 0, tx: 0, hist: Array.from({ length: 20 }, (_, i) => [((i * 7919) % 13) * 90000, ((i * 104729) % 11) * 5000]) }; if (myc.s.enabled) { m.rx += 60000; m.tx += 4000; m.hist[m.hist.length - 1] = [m.hist[m.hist.length - 1][0] + 60000, m.hist[m.hist.length - 1][1] + 4000] } return { ...m, hist: m.hist.map((b) => [...b]) } },
    webproxy_exit_countries: (a) => fkExit.view(a.host),
    webproxy_set_exit_country: (a) => fkExit.pin(a.host, a.iso),
    webproxy_new_identity: (a) => fkExit.rotate(a.host),
    webproxy_set_exit_default: (a) => fkExit.setDef(a.iso),
    webproxy_check_ip: (a) => new Promise((res, rej) => { const t = setTimeout(() => res(fkExit.check(a.host)), 1500); fkExit.cancel = () => { clearTimeout(t); rej('cancelled') } }),
    webproxy_check_ip_cancel: () => { fkExit.cancel?.(); return null },
    mycelium_hop_addrs: (a) => { const ips = ['88.198.12.7', '51.15.84.20', '185.70.42.9', '95.216.33.1']; return Object.fromEntries((a.ids || []).map((id, i) => [id, ips[i % ips.length]])) },
    net_conn_stats: () => {
      const on = myc.s.enabled, ep = (name, id, open, up = true) => ({ name, nodeId: up ? id.padEnd(64, '3') : '', up, open: up ? open : 0, openedMin: up ? 3 : 0, closedMin: up ? 2 : 0, relayChangesMin: 0, holepunchMin: up ? 1 : 0, direct: up ? open + 4 : 0, homeRelay: up ? 'https://relay-ru1.smartholdem.io/' : '' })
      return { endpoints: [ep('main', '5f3a', 9), ep('media', '91be', 2), ep('mycelium', '7c1e', 3, on)], connectErrorsMin: 0, relayErrorsMin: 0, lastError: '', storm: false, windowSecs: 60 }
    },
    get_trackers: () => ['159.69.188.60:7374', '116.202.32.250:7374'],
    get_seeders: () => ['116.202.32.250:6881'],
    get_seeders_auto: () => [],
    get_seeding_history_stats: () => [],
    get_relay_only: () => false,
    get_mobile_fallbacks: () => 'auto',
    get_swarm_enabled: () => true,
    get_dht_providers_enabled: () => true,
    get_dht_enabled: () => true,
    get_auto_update: () => true,
    swarm_top_seeders: () => [],
    dht_providers_summary: () => ({ enabled: true, seeders: 3, apps: 5, announcedApps: 2, beaconPort: 6881, dhtNodes: 42 }),
    get_relay_config: () => ({ enabled: true, urls: ['https://relay-ru1.smartholdem.io'] }),
    refresh_node_pool: () => [
      { host: 'node2.smartholdem.io', online: true, latencyMs: 118, height: 2345678, active: true, p2p: false },
      { host: 'node5.smartholdem.io', online: true, latencyMs: 240, height: 2345678, active: false, p2p: false },
      { host: 'node6.smartholdem.io', online: false, latencyMs: null, height: null, active: false, p2p: false },
    ],
    list_custom_nodes: () => ['api://747a93f8d56d361ba01a56265b1f10e746d7ab1962ee01189c43fae15481d4a0/sth-core', 'node2.smartholdem.io'],
    current_node: () => 'node2.smartholdem.io',
    get_client_version: () => '1.136.9',
    sync_status: () => ({ done: 5, total: 5, complete: true }),
    load_profile_manifest: () => ({ version: 1, public: { identity: acc.address, displayName: '', bio: '', avatarHash: '', rewardAddress: acc.address, isPublic: true, showDomains: true, updatedAt: new Date().toISOString() }, private: { encryptedSecret: '' }, statsCache: { appsPublished: 2, appsSeeded: 2 } }),
    list_apps: () => [
      { id: 'demo-blog', name: 'Micro Blog', description: 'P2P Blog', version: '1.0.0', owner: acc.address, status: 'published', tags: ['social'], sizeBytes: 1000, createdAt: new Date().toISOString(), hash: 'h1' },
      { id: 'demo-mail', name: 'Mail', description: 'E2E mail', version: '0.9.0', owner: acc.address, status: 'published', tags: [], sizeBytes: 1000, createdAt: new Date().toISOString(), hash: 'h2' },
    ],
    get_wallet_transactions: () => [],
    get_wallet_balance: () => ({ address: acc.address, balance: '40195.99', unconfirmed: '0' }),
    follow_stats: () => ({ subscribers: 1, following: 1, recentSubscribers: [{ address: 'SXk1', name: 'glass' }], followingList: [{ address: 'SXk2', name: 'slots' }] }),
    follow_stats_cached: () => ({ subscribers: 1, following: 1, recentSubscribers: [{ address: 'SXk1', name: 'glass' }], followingList: [{ address: 'SXk2', name: 'slots' }] }),
    domains_by_owner: () => [{ name: 'catsatoshi', zone: 'sth', owner: acc.address, appId: 'demo-blog', expires: Math.floor(Date.now() / 1000) + 86400 * 200 }],
    donor_tier: () => ({ tier: 1, count: 1, totalSth: '10' }),
    get_my_username: () => 'catsatoshi',
    username_of_address: () => 'catsatoshi',
    app_logo: () => null,
    official_lists: () => ({ developers: [], dapps: [], lastScan: 0 }),
    storage_usage: () => ({ used: 0, limit: 8e9, files: 0 }),
    ping_custom_node: () => ({ host: '', online: true, latencyMs: 90, height: 2345678, verified: true, p2p: false }),
    // мессенджер (?w=chat): одна группа с историей — для проверки мобильной шапки
    get_rooms: () => [{ id: 'g1', peerAddress: 'SGRP1', peerName: 'SmartHoldem', peerKey: '', lastText: 'Сегодня яндекс диск всё', lastTs: Date.now() - 60000, kind: 'group', groupSecret: 'x', unread: 0, owner: acc.address }, { id: 'd1', peerAddress: 'SPeerDm1xyz', peerName: 'alice', peerKey: '', lastText: 'привет', lastTs: Date.now() - 120000, kind: 'dm', groupSecret: null, unread: 0, owner: null }],
    chat_get_messages_page: () => ({ hasMore: false, messages: [
      { id: 'm1', room: 'g1', sender: 'SVA5YKKSK7abcdef', text: 'А у меня такое предложение, о переносе истории', ts: Date.now() - 500000, mine: false },
      { id: 'm2', room: 'g1', sender: acc.address, text: 'Но вроде бы все и правда подтянулось', ts: Date.now() - 400000, mine: true },
      { id: 'm3', room: 'g1', sender: 'SVA5YKKSK7abcdef', text: 'Сегодня яндекс диск всё, похоже надолго', ts: Date.now() - 60000, mine: false },
    ] }),
    gossip_room_peers: () => 5,
    join_group: () => ({ id: 'g1', peerAddress: 'SGRP1', peerName: 'SmartHoldem', peerKey: '', lastText: '', lastTs: Date.now(), kind: 'group', groupSecret: 'x', unread: 0, owner: acc.address }),
    get_group_policy: () => ({ entryFee: 0, paidEntry: false, inviteOnly: false, channelMode: false, pinned: null, banned: [], ts: 0 }),
    msg_host_available: () => true,
    open_room: (a) => ({ id: 'd1', peerAddress: a.target || 'SPeerDm1xyz', peerName: 'alice', peerKey: '', lastText: 'привет', lastTs: Date.now(), kind: 'dm', groupSecret: null, unread: 0, owner: null }),
    chat_key_status: () => ({ published: true, online: true }),
  }
  const invoke = async (cmd, args) => {
    if (cmd in table) return table[cmd](args || {})
    if (cmd.startsWith('plugin:event')) return 1
    if (cmd === 'take_pending_deep_link') return null
    if (cmd.startsWith('get_') || cmd.endsWith('_enabled')) return false
    if (/^(current_os|get_data_dir|get_my_username|get_my_chat_pubkey|translator_engine_label|domain_of_app)$/.test(cmd)) return ''
    return []
  }
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    transformCallback: (cb) => { const id = Math.floor(Math.random() * 1e9); window['_cb_' + id] = cb; return id },
    invoke,
  }
  window.__TAURI__ = { core: { invoke }, event: { listen: async () => () => {} } }
})()
