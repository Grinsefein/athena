import { get } from 'svelte/store';
import {
  auth, toasts, loading, downloading, queued, completed, progress, speed, eta,
  errorMsg, videoInfo, lastAnalyzedUrl, selectedFormat, formatSlide, selectedQuality,
  selectedPlaylistUrls, downloadId, downloadUrl, checkingUpdate, urlInput,
  cleanUrl, isValidUrl, connectSSE, closeSSE
} from './stores.js';

export async function checkConfig() {
  try {
    const response = await fetch('/api/config');
    const data = await response.json();
    if (data.success && data.data) {
      auth.setAuthEnabled(data.data.auth_enabled);
    }
  } catch (e) {
    console.error('Config fetch failed', e);
  }
}

export async function login(password) {
  if (!password) return { success: false, error: 'Passwort erforderlich' };
  try {
    const response = await fetch('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password })
    });
    const data = await response.json();
    if (data.success && data.data && data.data.token) {
      auth.setAuth(data.data.token);
      toasts.add('Erfolgreich angemeldet!', 'success');
      return { success: true };
    } else {
      return { success: false, error: data.error || 'Ungültiges Passwort' };
    }
  } catch (e) {
    return { success: false, error: 'Verbindungsfehler' };
  }
}

let analyzeController = null;
let pendingAuto = false;

export async function analyzeVideo(url) {
  const targetUrl = url ? cleanUrl(String(url).trim()) : '';
  if (!isValidUrl(targetUrl)) return;

  if (analyzeController) analyzeController.abort();
  analyzeController = new AbortController();
  const signal = analyzeController.signal;

  loading.set(true);
  errorMsg.set(null);
  closeSSE();
  downloading.set(false);
  completed.set(false);

  const authState = get(auth);

  try {
    const response = await fetch('/api/analyze', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(authState.token && { 'Authorization': `Bearer ${authState.token}` })
      },
      body: JSON.stringify({ url: targetUrl }),
      signal
    });

    let data;
    try { data = await response.json(); } catch (_) { throw new Error('Serverfehler'); }
    if (!response.ok || !data.success) {
      if (response.status === 401) {
        auth.setShowLogin(true);
        throw new Error('Nicht angemeldet');
      }
      throw new Error(data.error || 'Fehlgeschlagen');
    }

    videoInfo.set(data.data);
    lastAnalyzedUrl.set(targetUrl);
    selectedFormat.set('video');
    formatSlide.set('right');
    selectedQuality.set('best');

    if (data.data && data.data.playlist_videos) {
      selectedPlaylistUrls.set(data.data.playlist_videos.map(v => v.url));
    } else {
      selectedPlaylistUrls.set([]);
    }
    toasts.add('Analyse erfolgreich!', 'success');
  } catch (e) {
    if (e.name === 'AbortError') return;
    if (analyzeController && analyzeController.signal === signal && e.message !== 'Nicht angemeldet') {
      const msg = 'Video konnte nicht analysiert werden. Bitte Link prüfen.';
      errorMsg.set(msg);
      toasts.add(msg, 'error');
    }
  } finally {
    if (analyzeController && analyzeController.signal === signal) {
      loading.set(false);
      runPendingAutoAnalyze();
    }
  }
}

// Auto-analyze path used by the input debounce. Skips when the exact URL was
// already analyzed; defers (once) while another analysis is still running.
export function maybeAutoAnalyze(input) {
  const value = (input || '').trim();
  if (!value || !isValidUrl(value)) return;
  if (get(loading)) {
    pendingAuto = true;
    return;
  }
  if (get(lastAnalyzedUrl) === cleanUrl(value)) return;
  analyzeVideo(value);
}

function runPendingAutoAnalyze() {
  if (!pendingAuto) return;
  pendingAuto = false;
  maybeAutoAnalyze(get(urlInput));
}

export function resetPendingAnalysis() {
  pendingAuto = false;
  if (analyzeController) {
    analyzeController.abort();
    analyzeController = null;
  }
}

export async function startDownload() {
  const info = get(videoInfo);
  if (!info || get(downloading)) return;
  downloading.set(true);
  completed.set(false);
  errorMsg.set(null);
  progress.set(0);
  speed.set(null);
  eta.set(null);

  const authState = get(auth);
  const currentUrl = cleanUrl(get(urlInput).trim());
  const fmt = get(selectedFormat);
  const qual = get(selectedQuality);
  const playlistUrls = get(selectedPlaylistUrls);

  try {
    const response = await fetch('/api/download', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(authState.token && { 'Authorization': `Bearer ${authState.token}` })
      },
      body: JSON.stringify({
        url: currentUrl,
        format: fmt,
        quality: qual,
        playlist_urls: (info.playlist_videos && playlistUrls.length > 0) ? playlistUrls : null
      })
    });

    let data;
    try { data = await response.json(); } catch (_) { throw new Error('Serverfehler'); }
    if (!response.ok || !data.success) {
      if (response.status === 401) {
        auth.setShowLogin(true);
        throw new Error('Nicht angemeldet');
      }
      throw new Error(data.error || 'Start fehlgeschlagen');
    }

    const id = data.data.download_id;
    downloadId.set(id);
    queued.set(data.data.status === 'queued');
    connectSSE(id, authState.token);
  } catch (e) {
    downloading.set(false);
    if (e.message !== 'Nicht angemeldet') {
      const msg = 'Download-Fehler: ' + e.message;
      errorMsg.set(msg);
      toasts.add(msg, 'error');
    }
  }
}

export async function checkYtdlpUpdate() {
  checkingUpdate.set(true);
  const authState = get(auth);
  try {
    const response = await fetch('/api/ytdlp-update', {
      method: 'POST',
      headers: {
        ...(authState.token && { 'Authorization': `Bearer ${authState.token}` })
      }
    });
    const data = await response.json().catch(() => ({}));
    if (data.success) {
      const msg = data.data.status === 'updated' ? 'yt-dlp wurde aktualisiert!' : 'Bereits aktuell.';
      toasts.add(msg, 'success');
    } else {
      const detail = data.error ? String(data.error).replace(/^.*?: /, '').slice(0, 120) : '';
      toasts.add(detail ? `Update fehlgeschlagen: ${detail}` : 'Update fehlgeschlagen', 'error');
    }
  } catch (e) {
    toasts.add('Verbindungsfehler bei Update-Prüfung', 'error');
  } finally {
    checkingUpdate.set(false);
  }
}

export function resetApp() {
  resetPendingAnalysis();
  urlInput.set('');
  videoInfo.set(null);
  lastAnalyzedUrl.set(null);
  selectedFormat.set('video');
  formatSlide.set('right');
  selectedQuality.set('best');
  downloading.set(false);
  completed.set(false);
  queued.set(false);
  progress.set(0);
  speed.set(null);
  eta.set(null);
  errorMsg.set(null);
  downloadId.set(null);
  downloadUrl.set(null);
  selectedPlaylistUrls.set([]);
  closeSSE();
}
