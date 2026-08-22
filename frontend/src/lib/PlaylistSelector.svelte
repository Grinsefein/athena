<script>
  import { videoInfo, selectedPlaylistUrls } from '../stores.js';

  function selectAll(val) {
    if (val && $videoInfo && $videoInfo.playlist_videos) {
      selectedPlaylistUrls.set($videoInfo.playlist_videos.map(v => v.url));
    } else {
      selectedPlaylistUrls.set([]);
    }
  }
</script>

{#if $videoInfo && $videoInfo.playlist_videos && $videoInfo.playlist_videos.length > 0}
  <div class="playlist-panel reveal">
    <div class="playlist-head">
      <h4>Playlist-Videos auswählen</h4>
      <div class="playlist-actions">
        <button type="button" class="link-btn" on:click={() => selectAll(true)}>Alle</button>
        <button type="button" class="link-btn muted" on:click={() => selectAll(false)}>Keine</button>
      </div>
    </div>
    <div class="playlist-list">
      {#each $videoInfo.playlist_videos as video (video.id)}
        <label class="playlist-item">
          <input type="checkbox" value={video.url} bind:group={$selectedPlaylistUrls}>
          <span>{video.title}</span>
        </label>
      {/each}
    </div>
    <p class="playlist-count">{$selectedPlaylistUrls.length} von {$videoInfo.playlist_videos.length} Videos ausgewählt</p>
  </div>
{/if}
