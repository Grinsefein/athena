import { writable, derived } from 'svelte/store';

// Toast store
function createToastStore() {
  const { subscribe, update } = writable([]);

  return {
    subscribe,
    add: (message, type = 'info', timeout = 4000) => {
      const id = Date.now() + Math.random().toString(36).slice(2, 7);
      update(toasts => {
        const next = [...toasts, { id, message, type, visible: true }];
        return next.length > 4 ? next.slice(1) : next;
      });

      if (timeout) {
        setTimeout(() => {
          dismiss(id);
        }, timeout);
      }
      return id;
    },
    dismiss: (id) => dismiss(id)
  };

  function dismiss(id) {
    update(toasts =>
      toasts.map(t => t.id === id ? { ...t, visible: false } : t)
    );
    setTimeout(() => {
      update(toasts => toasts.filter(t => t.id !== id));
    }, 250);
  }
}

export const toasts = createToastStore();

// Dark mode store
function createThemeStore() {
  const initial = typeof localStorage !== 'undefined'
    ? localStorage.getItem('darkMode') === 'true' ||
      (localStorage.getItem('darkMode') === null && typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches)
    : false;

  const { subscribe, set, update } = writable(initial);

  return {
    subscribe,
    toggle: () => {
      update(current => {
        const next = !current;
        if (typeof localStorage !== 'undefined') {
          localStorage.setItem('darkMode', String(next));
        }
        if (typeof document !== 'undefined') {
          document.documentElement.classList.toggle('dark', next);
        }
        return next;
      });
    },
    init: () => {
      update(current => {
        if (typeof document !== 'undefined') {
          document.documentElement.classList.toggle('dark', current);
        }
        return current;
      });
    }
  };
}

export const theme = createThemeStore();

// Auth store
function createAuthStore() {
  const initialToken = typeof localStorage !== 'undefined' ? localStorage.getItem('athena_token') : null;
  const { subscribe, set, update } = writable({
    token: initialToken,
    isAuthenticated: !!initialToken,
    showLogin: false,
    authEnabled: false
  });

  return {
    subscribe,
    setAuth: (token) => {
      if (typeof localStorage !== 'undefined') {
        if (token) localStorage.setItem('athena_token', token);
        else localStorage.removeItem('athena_token');
      }
      update(s => ({ ...s, token, isAuthenticated: !!token, showLogin: false }));
    },
    setShowLogin: (show) => update(s => ({ ...s, showLogin: show })),
    setAuthEnabled: (enabled) => update(s => {
      const isAuthenticated = enabled ? !!s.token : true;
      const showLogin = enabled && !s.token;
      return { ...s, authEnabled: enabled, isAuthenticated, showLogin };
    })
  };
}

export const auth = createAuthStore();

// App Stores & API helpers
export const urlInput = writable('');
export const loading = writable(false);
export const downloading = writable(false);
export const queued = writable(false);
export const completed = writable(false);
export const progress = writable(0);
export const speed = writable(null);
export const eta = writable(null);
export const errorMsg = writable(null);
export const videoInfo = writable(null);
export const lastAnalyzedUrl = writable(null);
export const selectedFormat = writable('video');
export const formatSlide = writable('right');
export const selectedQuality = writable('best');
export const selectedPlaylistUrls = writable([]);
export const downloadId = writable(null);
export const downloadUrl = writable(null);
export const checkingUpdate = writable(false);

export function cleanUrl(url) {
  try {
    const urlObj = new URL(url);
    const videoId = urlObj.searchParams.get('v');
    if (videoId && (urlObj.hostname.includes('youtube.com') || urlObj.hostname.includes('youtu.be'))) {
      return `https://www.youtube.com/watch?v=${videoId}`;
    }
  } catch (_) {}
  return url;
}

export function isValidUrl(str) {
  try {
    const url = new URL(str);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch (_) {
    return false;
  }
}

export function formatDuration(val) {
  if (!val) return '';
  if (typeof val === 'string') {
    if (/^\d+$/.test(val)) val = parseInt(val, 10);
    else return val.includes(':') ? val : '';
  }
  const seconds = Number(val);
  if (isNaN(seconds) || seconds <= 0) return '';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  const mm = m.toString().padStart(h > 0 ? 2 : 1, '0');
  const ss = s.toString().padStart(2, '0');
  return h > 0 ? `${h}:${mm.padStart(2, '0')}:${ss} Std.` : `${mm}:${ss} Min.`;
}

// SSE Connection manager
let eventSource = null;
let sseRetries = 0;
const MAX_SSE_RETRIES = 3;

export function closeSSE() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
}

export function connectSSE(id, authToken) {
  closeSSE();
  sseRetries = 0;
  const tokenParam = authToken ? `?token=${encodeURIComponent(authToken)}` : '';
  const es = new EventSource(`/api/progress/${id}${tokenParam}`);
  eventSource = es;

  es.onmessage = (event) => {
    sseRetries = 0;
    let data;
    try { data = JSON.parse(event.data); } catch (_) { return; }

    if (data.status === 'error' || data.error) {
      const err = data.error || 'Fehler beim Herunterladen';
      errorMsg.set(err);
      toasts.add(err, 'error');
      downloading.set(false);
      closeSSE();
      return;
    }

    queued.set(data.status === 'queued');
    progress.set(Math.round(data.progress || 0));
    speed.set(data.speed || null);
    eta.set(data.eta || null);

    if (data.status === 'completed') {
      downloading.set(false);
      completed.set(true);
      downloadUrl.set(data.download_url);
      toasts.add('Download abgeschlossen!', 'success');
      closeSSE();
    }
  };

  es.onerror = () => {
    let isCompleted = false;
    completed.subscribe(v => isCompleted = v)();
    if (isCompleted || sseRetries >= MAX_SSE_RETRIES) {
      closeSSE();
      return;
    }
    sseRetries += 1;
  };
}
