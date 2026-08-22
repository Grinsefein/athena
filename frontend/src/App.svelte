<script>
  import { onMount } from 'svelte';
  import TopBar from './lib/TopBar.svelte';
  import Toasts from './lib/Toasts.svelte';
  import LoginModal from './lib/LoginModal.svelte';
  import MediaCard from './lib/MediaCard.svelte';
  import FormatSelector from './lib/FormatSelector.svelte';
  import PlaylistSelector from './lib/PlaylistSelector.svelte';
  import DownloadProgress from './lib/DownloadProgress.svelte';

  import { theme, urlInput, loading, videoInfo, errorMsg, cleanUrl, isValidUrl } from './stores.js';
  import { checkConfig, analyzeVideo } from './api.js';

  let debounceTimeout;

  onMount(async () => {
    theme.init();
    await checkConfig();

    const params = new URLSearchParams(window.location.search);
    const shared = params.get('share') || params.get('url');
    if (shared && isValidUrl(shared)) {
      history.replaceState({}, '', window.location.pathname);
      urlInput.set(shared.trim());
      analyzeVideo(shared.trim());
      return;
    }
  });

  function handleInputKeydown(e) {
    if (e.key === 'Enter') {
      analyzeVideo($urlInput);
    }
  }

  function handlePaste(event) {
    const pastedText = (event.clipboardData || window.clipboardData).getData('text');
    if (pastedText) {
      const trimmed = pastedText.trim();
      urlInput.set(trimmed);
      if (isValidUrl(trimmed)) {
        setTimeout(() => analyzeVideo(trimmed), 10);
      }
    }
  }

  $: {
    if ($urlInput && isValidUrl($urlInput)) {
      clearTimeout(debounceTimeout);
      debounceTimeout = setTimeout(() => {
        if (!$loading && (!$videoInfo || $videoInfo.url !== cleanUrl($urlInput))) {
          analyzeVideo($urlInput);
        }
      }, 800);
    }
  }
</script>

<TopBar />
<Toasts />
<LoginModal />

<main class="shell" class:results-mode={!!$videoInfo}>
  <div class="wrap">
    <!-- Header -->
    <header class="hero">
      <div class="logo-badge" aria-hidden="true">
        <svg viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
      </div>
      <h1>Athena Pi</h1>
      <p class="tagline">Einfach, schnell, zuverlässig.</p>
    </header>

    <!-- Main Card -->
    <section class="card" aria-label="Video Downloader">
      <div class="card-body">
        <!-- URL Input -->
        <div class="input-row">
          <input
            type="url"
            class="url-input"
            bind:value={$urlInput}
            on:keydown={handleInputKeydown}
            on:paste={handlePaste}
            placeholder="Video-Link hier einfügen..."
            aria-label="Video-URL"
            inputmode="url"
            autocapitalize="off"
            autocorrect="off"
            spellcheck="false"
            enterkeyhint="go"
            autocomplete="off"
            disabled={$loading}
          >
          <button
            type="button"
            class="go-btn"
            on:click={() => analyzeVideo($urlInput)}
            disabled={!$urlInput || $loading}
            aria-label="Video analysieren"
          >
            {#if !$loading}
              <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M14 5l7 7m0 0l-7 7m7-7H3"/>
              </svg>
            {:else}
              <span class="spin sm" aria-hidden="true" style="color:#fff;"></span>
            {/if}
          </button>
        </div>

        <!-- Error State -->
        {#if $errorMsg}
          <div class="alert-error reveal" role="alert">{$errorMsg}</div>
        {/if}

        <!-- Result Section -->
        {#if $videoInfo}
          <div class="result reveal">
            <MediaCard />
            <FormatSelector />
            <PlaylistSelector />
            <DownloadProgress />
          </div>
        {/if}
      </div>
    </section>
  </div>

  <footer class="footer">&copy; 2026 Athena Pi Downloader &bull; Built with Rust</footer>
</main>
