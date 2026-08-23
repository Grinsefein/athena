import { writable, derived, get } from 'svelte/store';

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
export const aborting = writable(false);
export const wantLyrics = writable(false);
export const lyricsPlain = writable(null);
export const lyricsSynced = writable(null);

export function clearLyrics() {
  lyricsPlain.set(null);
  lyricsSynced.set(null);
}

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
let sseRetryTimer = null;

const MAX_SSE_RETRIES = 3;
const SSE_BACKOFF_BASE_MS = 1000;
const SSE_BACKOFF_MAX_MS = 8000;

export function closeSSE() {
  if (sseRetryTimer) {
    clearTimeout(sseRetryTimer);
    sseRetryTimer = null;
  }
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
}

/// Reconnect with exponential backoff (1s -> 2s -> 4s ... capped at 8s).
///
/// The native EventSource would retry immediately and hammer the server on
/// flaky Wi-Fi; closing it and re-opening on a timer gives the network time
/// to recover instead of burning all retries within milliseconds.
function scheduleSSERetry(id, authToken, opts) {
  const delay = Math.min(
    SSE_BACKOFF_BASE_MS * 2 ** (sseRetries - 1),
    SSE_BACKOFF_MAX_MS
  );
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
  sseRetryTimer = setTimeout(() => {
    sseRetryTimer = null;
    openStream(id, authToken, opts);
  }, delay);
}

function openStream(id, authToken, opts = {}) {
  const tokenParam = authToken ? `?token=${encodeURIComponent(authToken)}` : '';
  const es = new EventSource(`/api/progress/${id}${tokenParam}`);
  eventSource = es;

  es.onmessage = (event) => {
    sseRetries = 0;
    let data;
    try { data = JSON.parse(event.data); } catch (_) { return; }

    if (data.status === 'aborted') {
      downloading.set(false);
      queued.set(false);
      speed.set(null);
      eta.set(null);
      downloadId.set(null);
      persistSession();
      closeSSE();
      return;
    }

    if (data.status === 'error' || data.error) {
      const err = data.error || 'Fehler beim Herunterladen';

      // After a refresh the download may already be gone (served/cleaned up).
      // Recover quietly instead of scaring the user with an error box.
      if (err === 'Download not found') {
        downloading.set(false);
        queued.set(false);
        downloadId.set(null);
        downloadUrl.set(null);
        toasts.add('Download nicht mehr verfügbar', 'info');
        persistSession();
        closeSSE();
        return;
      }

      errorMsg.set(err);
      toasts.add(err, 'error');
      downloading.set(false);
      downloadId.set(null);
      clearLyrics();
      persistSession();
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
      lyricsPlain.set(data.lyrics_plain || null);
      lyricsSynced.set(data.lyrics_synced || null);
      toasts.add('Download abgeschlossen!', 'success');
      persistSession();
      closeSSE();
    }
  };

  es.onerror = () => {
    let isCompleted = false;
    completed.subscribe(v => isCompleted = v)();
    if (isCompleted || sseRetries >= MAX_SSE_RETRIES) {
      closeSSE();
      if (!isCompleted && !opts.reattach) {
        // Never leave the UI stuck on a dead connection.
        errorMsg.set('Verbindung zum Server verloren.');
        toasts.add('Verbindung zum Server verloren.', 'error');
      }
      downloading.set(false);
      queued.set(false);
      speed.set(null);
      eta.set(null);
      if (!isCompleted) {
        downloadId.set(null);
        persistSession();
      }
      return;
    }
    sseRetries += 1;
    scheduleSSERetry(id, authToken, opts);
  };
}

/// Public entry point: binds the SSE stream to a download and resets the
/// retry budget (used for fresh downloads and session re-attach).
export function connectSSE(id, authToken, opts = {}) {
  closeSSE();
  sseRetries = 0;
  openStream(id, authToken, opts);
}

// ---------------------------------------------------------------------------
// Tab-bound session state (sessionStorage): survives a page refresh within
// the same tab so an accidental reload never loses the analysis result or
// the live progress of a running download. The SSE stream re-attaches to the
// server-side download task and resumes real-time updates.
// ---------------------------------------------------------------------------
const SESSION_KEY = 'athena_tab_state_v1';

function safeSessionGet() {
  try {
    const raw = sessionStorage.getItem(SESSION_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch (_) {
    return null;
  }
}

export function persistSession() {
  try {
    const info = get(videoInfo);
    const id = get(downloadId);
    const done = get(completed);
    if (!info && !id) {
      sessionStorage.removeItem(SESSION_KEY);
      return;
    }
    sessionStorage.setItem(SESSION_KEY, JSON.stringify({
      url: get(urlInput),
      videoInfo: info,
      lastAnalyzedUrl: get(lastAnalyzedUrl),
      selectedFormat: get(selectedFormat),
      selectedQuality: get(selectedQuality),
      selectedPlaylistUrls: get(selectedPlaylistUrls),
      downloadId: id,
      downloadUrl: get(downloadUrl),
      completed: done,
      wantLyrics: get(wantLyrics),
      lyricsPlain: get(lyricsPlain),
      lyricsSynced: get(lyricsSynced),
    }));
  } catch (_) {}
}

export function clearSession() {
  try { sessionStorage.removeItem(SESSION_KEY); } catch (_) {}
}

/// Binds the UI to a server-side download task and re-attaches the SSE
/// stream. Shared by same-tab session restore and cross-tab recovery.
function attachRunningDownload(id) {
  downloadId.set(id);
  speed.set(null);
  eta.set(null);
  downloading.set(true);
  queued.set(true);
  progress.set(0);
  connectSSE(id, get(auth).token, { reattach: true });
}

/// Restores analysis results and any running download into the stores.
/// Returns true when something was restored.
export function restoreSession() {
  let snap;
  try {
    snap = safeSessionGet();
  } catch (_) {
    return false;
  }
  if (!snap) return false;

  let restored = false;

  if (snap.url) urlInput.set(snap.url);

  if (snap.videoInfo) {
    videoInfo.set(snap.videoInfo);
    lastAnalyzedUrl.set(snap.lastAnalyzedUrl || null);
    selectedFormat.set(snap.selectedFormat || 'video');
    formatSlide.set((snap.selectedFormat || 'video') === 'audio' ? 'left' : 'right');
    selectedQuality.set(snap.selectedQuality || 'best');
    selectedPlaylistUrls.set(
      Array.isArray(snap.selectedPlaylistUrls) ? snap.selectedPlaylistUrls : []
    );
    wantLyrics.set(!!snap.wantLyrics);
    lyricsPlain.set(snap.lyricsPlain || null);
    lyricsSynced.set(snap.lyricsSynced || null);
    restored = true;
  }

  if (snap.downloadId) {
    if (snap.completed && snap.downloadUrl) {
      completed.set(true);
      downloading.set(false);
      progress.set(100);
      downloadUrl.set(snap.downloadUrl);
    } else {
      // Re-bind to the still-running server-side download; the SSE stream
      // pushes the current status/progress within ~500ms of connecting.
      attachRunningDownload(snap.downloadId);
    }
    restored = true;
  }

  return restored;
}

// ---------------------------------------------------------------------------
// Cross-tab recovery: an active download is mirrored to localStorage so it
// survives the tab being closed entirely (sessionStorage dies with the tab).
// Reopening the app anywhere offers to re-bind to the still-running job.
// The subscription keeps the snapshot in sync with every store transition
// (start, terminal SSE events, abort, reset) without touching call sites.
// ---------------------------------------------------------------------------
const ACTIVE_KEY = 'athena_active_dl_v1';
const ACTIVE_TTL_MS = 2 * 60 * 60 * 1000;

function writeActiveMirror() {
  try {
    const id = get(downloadId);
    if (!id || get(completed)) {
      localStorage.removeItem(ACTIVE_KEY);
      return;
    }
    localStorage.setItem(ACTIVE_KEY, JSON.stringify({
      startedAt: Date.now(),
      url: get(urlInput),
      videoInfo: get(videoInfo),
      selectedFormat: get(selectedFormat),
      selectedQuality: get(selectedQuality),
      selectedPlaylistUrls: get(selectedPlaylistUrls),
      wantLyrics: get(wantLyrics),
      downloadId: id,
    }));
  } catch (_) {}
}

downloadId.subscribe(writeActiveMirror);
completed.subscribe(writeActiveMirror);

/// Recovers a download that was started in another (closed) tab. Only used
/// when same-tab session restore found nothing, so a plain reload never
/// takes this path.
export function restoreCrossTabSession() {
  let snap;
  try {
    const raw = localStorage.getItem(ACTIVE_KEY);
    snap = raw ? JSON.parse(raw) : null;
  } catch (_) {
    return false;
  }
  if (!snap || !snap.downloadId || !snap.videoInfo) return false;

  if (!snap.startedAt || Date.now() - snap.startedAt > ACTIVE_TTL_MS) {
    try { localStorage.removeItem(ACTIVE_KEY); } catch (_) {}
    return false;
  }

  urlInput.set(snap.url || '');
  videoInfo.set(snap.videoInfo);
  lastAnalyzedUrl.set(snap.url || null);
  selectedFormat.set(snap.selectedFormat || 'video');
  formatSlide.set((snap.selectedFormat || 'video') === 'audio' ? 'left' : 'right');
  selectedQuality.set(snap.selectedQuality || 'best');
  selectedPlaylistUrls.set(
    Array.isArray(snap.selectedPlaylistUrls) ? snap.selectedPlaylistUrls : []
  );
  wantLyrics.set(!!snap.wantLyrics);

  attachRunningDownload(snap.downloadId);
  toasts.add('Laufenden Download wiederhergestellt', 'info');
  return true;
}
