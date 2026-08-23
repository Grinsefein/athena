<script>
  import { videoInfo, selectedFormat, formatSlide, selectedQuality, wantLyrics } from '../stores.js';

  function setFormat(type) {
    if ($selectedFormat !== type) {
      const order = ['video', 'audio'];
      formatSlide.set(order.indexOf(type) > order.indexOf($selectedFormat) ? 'right' : 'left');
    }
    selectedFormat.set(type);
    selectedQuality.set('best');
  }

  $: filteredFormats = (() => {
    if (!$videoInfo || !$videoInfo.formats) return [];
    const seen = new Set();
    return $videoInfo.formats.filter(f => {
      if (f.media_type !== $selectedFormat) return false;
      const key = `${f.label}|${f.format}|${f.filesize || ''}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  })();

  $: currentFormatObj = filteredFormats.find(f => f.quality === $selectedQuality) || null;

  function optionLabel(fmt) {
    return fmt.filesize ? `${fmt.label} · ${fmt.filesize}` : fmt.label;
  }
</script>

<div class="format-grid">
  <!-- Format Toggle -->
  <div>
    <label class="label" for="formatSegment">Format</label>
    <div id="formatSegment" class="segmented" role="group" aria-label="Format wählen">
      <div class="segmented-indicator" style={`transform: translateX(${$selectedFormat === 'audio' ? '100%' : '0%'})`} aria-hidden="true"></div>
      <button type="button" on:click={() => setFormat('video')} aria-pressed={$selectedFormat === 'video'}>Video</button>
      <button type="button" on:click={() => setFormat('audio')} aria-pressed={$selectedFormat === 'audio'}>Audio</button>
    </div>
  </div>

  <!-- Quality Select -->
  <div>
    <label class="label" for="qualitySelect">Qualität</label>
    <div class="select-row sel-slide-{$formatSlide}">
      <select id="qualitySelect" class="quality-select" bind:value={$selectedQuality} aria-label="Qualität wählen">
        {#each filteredFormats as fmt (fmt.quality)}
          <option value={fmt.quality}>{optionLabel(fmt)}</option>
        {/each}
      </select>
      <svg class="select-chevron" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M19 9l-7 7-7-7"/>
      </svg>
    </div>
    {#if currentFormatObj && currentFormatObj.filesize}
      <p class="size-hint">Ungefähre Dateigröße: {currentFormatObj.filesize}</p>
    {/if}
  </div>
</div>

<!-- Lyrics option (audio only) -->
{#if $selectedFormat === 'audio'}
  <label class="check-row" title="Songtext über lrclib suchen, in die Datei einbetten und als .lrc/.txt anbieten">
    <input type="checkbox" bind:checked={$wantLyrics}>
    <span class="check-row-text">Songtext (Lyrics) mitladen</span>
  </label>
{/if}
