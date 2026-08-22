<script>
  import { videoInfo, formatDuration } from '../stores.js';
</script>

{#if $videoInfo}
  <div class="media">
    <div class="media-blur" style={$videoInfo.thumbnail ? `background-image:url('${$videoInfo.thumbnail}')` : ''} aria-hidden="true"></div>
    <img src={$videoInfo.thumbnail || ''} alt="" class="thumb" loading="lazy" decoding="async">
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
