<script>
  import { downloading, queued, progress, completed, speed, eta, downloadUrl, auth } from '../stores.js';
  import { startDownload, resetApp } from '../api.js';

  $: fullDownloadUrl = $downloadUrl
    ? $downloadUrl + ($auth.token ? ($downloadUrl.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent($auth.token) : '')
    : '#';
</script>

<div class="dl-wrap">
  <button
    type="button"
    class="dl-btn"
    on:click={startDownload}
    disabled={$downloading || $completed}
    aria-busy={$downloading}
  >
    {#if $downloading && !$queued && $progress > 0}
      <div class="dl-progress" style="width:{$progress}%" aria-hidden="true"></div>
    {/if}
    <div class="dl-content">
      <div class="dl-main">
        {#if $downloading}
          <span class="spin" aria-hidden="true"></span>
        {/if}
        {#if !$downloading && !$completed}
          <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
          </svg>
        {/if}
        {#if $completed}
          <svg class="done-icon" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M5 13l4 4L19 7"/>
          </svg>
        {/if}
        <span>
          {$downloading
            ? ($queued ? 'In der Warteschlange...' : `Wird heruntergeladen (${$progress}%)`)
            : ($completed ? 'Abgeschlossen' : 'Download starten')}
        </span>
      </div>
      {#if $downloading && !$queued && ($speed || $eta)}
        <div class="dl-sub">
          {#if $speed}<span>{$speed}</span>{/if}
          {#if $speed && $eta}<span>•</span>{/if}
          {#if $eta}<span>ETA {$eta}</span>{/if}
        </div>
      {/if}
    </div>
  </button>
</div>

{#if $completed}
  <div class="actions reveal">
    <a href={fullDownloadUrl} download class="btn-save">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/></svg>
      Datei speichern
    </a>
    <button type="button" class="btn-ghost" on:click={resetApp}>
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 6v6m0 0v6m0-6h6m-6 0H6"/></svg>
      Neues Video
    </button>
  </div>
{/if}
