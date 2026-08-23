<script>
  import { theme, checkingUpdate } from '../stores.js';
  import { checkYtdlpUpdate, resetApp } from '../api.js';
</script>

<div class="topbar-left">
  <a
    href="/"
    class="icon-btn"
    on:click={resetApp}
    aria-label="Zur Startseite"
    title="Zur Startseite"
  >
    <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 10.5L12 3l9 7.5M5 9.5V21h5v-6h4v6h5V9.5"/>
    </svg>
  </a>
</div>

<div class="topbar">
  <button
    type="button"
    on:click={checkYtdlpUpdate}
    disabled={$checkingUpdate}
    class="icon-btn"
    aria-label="yt-dlp aktualisieren"
    title="yt-dlp aktualisieren"
  >
    {#if !$checkingUpdate}
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"/>
      </svg>
    {:else}
      <span class="spin sm" aria-hidden="true"></span>
    {/if}
  </button>
  <button
    type="button"
    on:click={() => theme.toggle()}
    class="icon-btn"
    aria-label={$theme ? 'Light Mode aktivieren' : 'Dark Mode aktivieren'}
    title="Design wechseln"
  >
    {#if !$theme}
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z"/></svg>
    {:else}
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z"/></svg>
    {/if}
  </button>
</div>
