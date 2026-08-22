<script>
  import { onMount, tick } from 'svelte';
  import TopBar from './lib/TopBar.svelte';
  import Toasts from './lib/Toasts.svelte';
  import LoginModal from './lib/LoginModal.svelte';
  import MediaCard from './lib/MediaCard.svelte';
  import FormatSelector from './lib/FormatSelector.svelte';
  import PlaylistSelector from './lib/PlaylistSelector.svelte';
  import DownloadProgress from './lib/DownloadProgress.svelte';

  import {
    theme, urlInput, loading, videoInfo, errorMsg, isValidUrl, restoreSession
  } from './stores.js';
  import { checkConfig, analyzeVideo, cancelAnalyze } from './api.js';

  let isInputFocused = false;
  // On small screens the input row is replaced by a read-only label once
  // results are shown; tapping the label re-opens the editor.
  let editingUrl = false;
  let isMobileViewport = false;

  $: resultsMode = !!$videoInfo;
  $: hideInput = isMobileViewport && resultsMode && !editingUrl && !$loading;

  onMount(() => {
    theme.init();

    const mq = window.matchMedia('(max-width: 639px)');
    const applyViewport = () => (isMobileViewport = mq.matches);
    applyViewport();
    mq.addEventListener('change', applyViewport);
    return () => mq.removeEventListener('change', applyViewport);
  });

  onMount(async () => {
    await checkConfig();

    const params = new URLSearchParams(window.location.search);
    const shared = params.get('share') || params.get('url');
    if (shared && isValidUrl(shared)) {
      history.replaceState({}, '', window.location.pathname);
      urlInput.set(shared.trim());
      analyzeVideo(shared.trim());
      return;
    }

    // Re-bind to this tab's previous state (results + running download).
    restoreSession();
  });

  function handleInputKeydown(e) {
    if (e.key === 'Enter') {
      editingUrl = false;
      analyzeVideo($urlInput);
    }
  }

  function handlePaste(event) {
    const pastedText = (event.clipboardData || window.clipboardData).getData('text');
    if (pastedText && isValidUrl(pastedText.trim())) {
      urlInput.set(pastedText.trim());
    }
  }

  function handleGoButton() {
    editingUrl = false;
    if ($loading) {
      cancelAnalyze();
    } else {
      analyzeVideo($urlInput);
    }
  }

  async function openUrlEditor() {
    editingUrl = true;
    await tick();
    document.querySelector('.url-input')?.focus();
  }
</script>

<TopBar />
<Toasts />
<LoginModal />

<main class="shell" class:results-mode={!!$videoInfo} class:focus-mode={isInputFocused}>
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
        <div class="input-row" class:input-hidden={hideInput}>
          <input
            type="url"
            class="url-input"
            bind:value={$urlInput}
            on:keydown={handleInputKeydown}
            on:paste={handlePaste}
            on:focus={() => (isInputFocused = true)}
            on:blur={() => (isInputFocused = false)}
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
            class:cancel-mode={$loading}
            on:click={handleGoButton}
            disabled={!$loading && !$urlInput}
            aria-label={$loading ? 'Analyse abbrechen' : 'Video analysieren'}
            title={$loading ? 'Analyse abbrechen' : 'Video analysieren'}
          >
            {#if !$loading}
              <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M14 5l7 7m0 0l-7 7m7-7H3"/>
              </svg>
            {:else}
              <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M6 6l12 12M18 6L6 18"/>
              </svg>
            {/if}
          </button>
        </div>

        <!-- Read-only URL label (mobile, while results are shown) -->
        {#if hideInput}
          <button
            type="button"
            class="url-label reveal"
            on:click={openUrlEditor}
            title={$videoInfo?.title || $urlInput}
          >
            <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15.232 5.232l3.536 3.536m-2.036-5.036a2.5 2.5 0 113.536 3.536L6.5 21.036H3v-3.572L16.732 3.732z"/>
            </svg>
            <span class="url-label-text">{$videoInfo?.title || $urlInput}</span>
          </button>
        {/if}

        <!-- Analyze Status -->
        {#if $loading}
          <div class="analyze-status reveal" role="status" aria-live="polite">
            <span class="spin sm" aria-hidden="true"></span>
            <span>Video wird analysiert …</span>
          </div>
        {/if}

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
