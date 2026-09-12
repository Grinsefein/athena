const CACHE_NAME = 'athena-v10';
const CORE_ASSETS = [
  '/',
  '/manifest.json',
  '/app-icon.png'
];

self.addEventListener('install', event => {
  event.waitUntil(
    caches.open(CACHE_NAME)
      .then(cache => cache.addAll(CORE_ASSETS))
      .then(() => self.skipWaiting())
      .catch(err => console.warn('Precache failed:', err))
  );
});

self.addEventListener('activate', event => {
  event.waitUntil(
    caches.keys()
      .then(keys => Promise.all(
        keys.filter(key => key !== CACHE_NAME).map(key => caches.delete(key))
      ))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', event => {
  const { request } = event;

  if (request.method !== 'GET') return;

  const url = new URL(request.url);

  if (url.origin === self.location.origin && url.pathname.startsWith('/api/')) {
    return;
  }

  // Navigation requests: network first, fall back to cached shell when offline.
  // Never resolve with undefined: fall back to a network error so the
  // browser shows its offline page instead of throwing on 'undefined'.
  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request)
        .then(response => {
          const copy = response.clone();
          caches.open(CACHE_NAME).then(cache => cache.put('/', copy));
          return response;
        })
        .catch(() => caches.match('/').then(hit => hit || Response.error()))
    );
    return;
  }

  // Static assets (same-origin + CDN): stale-while-revalidate.
  // Never resolve with undefined: if the network fails (offline, blocked
  // CDN host, ...) and nothing is cached, let the promise reject so the
  // browser runs its normal error path (e.g. <img on:error> fallback)
  // instead of throwing "non-Response value 'undefined'".
  event.respondWith(
    caches.match(request).then(cached => {
      const network = fetch(request).then(response => {
        if (response && (response.ok || response.type === 'opaque')) {
          const copy = response.clone();
          caches.open(CACHE_NAME).then(cache => cache.put(request, copy));
        }
        return response;
      });
      // Serve stale instantly while revalidating in the background.
      if (cached) return cached;
      return network;
    })
  );
});
