import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', redirect: '/dashboard' },
    { path: '/dashboard', name: 'dashboard', component: () => import('@/views/Dashboard.vue') },
    { path: '/apps', name: 'apps', component: () => import('@/views/MyApplications.vue') },
    { path: '/discover', name: 'discover', component: () => import('@/views/Discovered.vue') },
    { path: '/installed', name: 'installed', component: () => import('@/views/Installed.vue') },
    { path: '/network', name: 'network', component: () => import('@/views/NetworkMap.vue') },
    { path: '/resolve/:id', name: 'resolve', component: () => import('@/views/Resolve.vue') },
    { path: '/keys', name: 'keys', component: () => import('@/views/Keys.vue') },
    { path: '/settings', name: 'settings', component: () => import('@/views/Settings.vue') },
    { path: '/nodes', name: 'nodes', component: () => import('@/views/NodePool.vue') },
    { path: '/ai-computer', name: 'ai-computer', component: () => import('@/views/AiComputer.vue') },
    { path: '/profile', name: 'profile', component: () => import('@/views/UserProfile.vue') },
    { path: '/profile/:identity', name: 'profile-view', component: () => import('@/views/UserProfile.vue') },
    { path: '/nameservice', name: 'nameservice', component: () => import('@/views/NameService.vue') },
    { path: '/domarket', name: 'domarket', component: () => import('@/views/DomainsMarket.vue') },
    { path: '/files', name: 'files', component: () => import('@/views/FileExchange.vue') },
    { path: '/myfiles', name: 'myfiles', component: () => import('@/views/MyFiles.vue') },
    { path: '/torrents', name: 'torrents', component: () => import('@/views/Torrents.vue') },
    { path: '/music', name: 'music', component: () => import('@/views/Music.vue') },
    { path: '/storages', name: 'storages', component: () => import('@/views/Storages.vue') },
    { path: '/vault', name: 'vault', component: () => import('@/views/CryptoVault.vue') },
    { path: '/seeders', name: 'seeders', component: () => import('@/views/Seeders.vue') },
    { path: '/album/:id', name: 'album', component: () => import('@/views/AlbumView.vue') },
    { path: '/file/:cid', name: 'file', component: () => import('@/views/FileDownload.vue') },
    { path: '/api-view', name: 'api-view', component: () => import('@/views/ApiView.vue') },
    { path: '/dev', name: 'dev', component: () => import('@/views/DevDashboard.vue') },
    { path: '/dev/new', name: 'dev-new', component: () => import('@/views/DevNewRepo.vue') },
    { path: '/dev/:username', name: 'dev-user', component: () => import('@/views/DevUserProfile.vue') },
    { path: '/dev/:username/:repo', name: 'dev-repo', component: () => import('@/views/DevRepoView.vue') },
    {
      path: '/app/:address',
      name: 'app-viewer',
      component: () => import('@/views/AppViewer.vue'),
    },
    { path: '/:pathMatch(.*)*', redirect: '/dashboard' },
  ],
})

export default router
