<script>
  import { toasts, lyricsPlain, lyricsSynced, videoInfo } from '../stores.js';

  let mode = 'plain';

  $: hasSynced = !!$lyricsSynced;
  $: hasPlain = !!$lyricsPlain;
  $: effectiveMode = mode === 'synced' && !hasSynced ? 'plain' : mode;
  $: plainText = $lyricsPlain || ($lyricsSynced ? stripTimestamps($lyricsSynced) : '');
  $: currentText = effectiveMode === 'synced' ? $lyricsSynced : plainText;
  $: fileBase = safeName($videoInfo?.title || 'lyrics');

  function stripTimestamps(lrc) {
    return lrc
      .split('\n')
      .map(line => line.replace(/^\s*\[\d{1,2}:\d{1,2}(?:[.:]\d{1,3})?\]\s*/, '').trim())
      .filter(Boolean)
      .join('\n');
  }

  function safeName(name) {
    return name.replace(/[\\/:*?"<>|]+/g, '_').slice(0, 120) || 'lyrics';
  }

  async function copyLyrics() {
    try {
      await navigator.clipboard.writeText(currentText);
      toasts.add(effectiveMode === 'synced' ? 'Songtext mit Zeitstempeln kopiert!' : 'Songtext kopiert!', 'success');
    } catch (_) {
      toasts.add('Kopieren nicht möglich', 'error');
    }
  }

  function downloadLrc() {
    const content = $lyricsSynced || plainText;
    const blob = new Blob([content + '\n'], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${fileBase}.lrc`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }

  function downloadTxt() {
    const blob = new Blob([plainText + '\n'], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${fileBase}.txt`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }
</script>

{#if hasPlain || hasSynced}
  <div class="lyrics-panel reveal">
    <div class="lyrics-head">
      <span class="label">Songtext</span>
      <div class="lyrics-tabs" role="tablist" aria-label="Songtext-Anzeige">
        <button
          type="button"
          role="tab"
          aria-selected={effectiveMode === 'plain'}
          on:click={() => (mode = 'plain')}
        >Ohne Zeitstempel</button>
        {#if hasSynced}
          <button
            type="button"
            role="tab"
            aria-selected={effectiveMode === 'synced'}
            on:click={() => (mode = 'synced')}
          >Mit Zeitstempeln</button>
        {/if}
      </div>
    </div>

    <pre class="lyrics-body">{currentText}</pre>

    <div class="lyrics-actions">
      <button type="button" class="lyrics-btn" on:click={copyLyrics}>
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h8a2 2 0 002-2v-2m-6-12h8a2 2 0 012 2v8m-10-4h6"/>
        </svg>
        Kopieren
      </button>
      <button type="button" class="lyrics-btn" on:click={downloadLrc}>
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
        </svg>
        .lrc speichern
      </button>
      <button type="button" class="lyrics-btn" on:click={downloadTxt}>
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 12h6m-6 4h6M9 8h6M5 21h14a1 1 0 001-1V4a1 1 0 00-1-1H5a1 1 0 00-1 1v16a1 1 0 001 1z"/>
        </svg>
        .txt speichern
      </button>
    </div>
    <p class="lyrics-hint">Der Text wurde außerdem direkt in die Audiodatei eingebettet.</p>
  </div>
{/if}
