<script>
  import { videoInfo, formatDuration } from '../stores.js';

  // Neutral placeholder shown while no thumbnail exists or the CDN URL
  // failed to load (expired/private videos), so the media row never shows
  // the browser's broken-image icon.
  const THUMB_FALLBACK =
    "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 160 90'%3E%3Crect width='160' height='90' fill='%23e2e8f0'/%3E%3Cpath d='M68 28 L68 62 L98 45 Z' fill='%2394a3b8'/%3E%3C/svg%3E";

  let thumbFailed = false;

  // New analysis result -> give its thumbnail a fresh chance
  $: $videoInfo, thumbFailed = false;

  function normalizeThumbUrl(url) {
    if (!url) return null;
    // Protocol-relative URLs ("//i.ytimg.com/...") break inside url('...')
    // and resolve against the app origin instead of the CDN.
    return url.startsWith('//') ? `https:${url}` : url;
  }

  $: thumbnail = normalizeThumbUrl($videoInfo?.thumbnail);
  $: hasRealThumb = !!thumbnail && !thumbFailed;
  $: thumbSrc = hasRealThumb ? thumbnail : THUMB_FALLBACK;
</script>

{#if $videoInfo}
  <div class="media">
    {#if hasRealThumb}
      <div class="media-blur" style={`background-image:url('${thumbnail}')`} aria-hidden="true"></div>
    {/if}
    <img
      src={thumbSrc}
      alt=""
      class="thumb"
      loading="lazy"
      decoding="async"
      on:error={() => (thumbFailed = true)}
    >
    <div class="media-info">
      <h3 class="media-title">{$videoInfo.title}</h3>
      <p class="media-meta">
        <span class="author">{$videoInfo.author}</span>
        {#if $videoInfo.duration}
          <span class="meta-dot">&bull;</span>
          <span class="dur">{formatDuration($videoInfo.duration)}</span>
        {/if}
      </p>
    </div>
  </div>
{/if}
