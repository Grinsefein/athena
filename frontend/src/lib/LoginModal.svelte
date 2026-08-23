<script>
  import { onDestroy } from 'svelte';
  import { auth } from '../stores.js';
  import { login } from '../api.js';

  let password = '';
  let loggingIn = false;
  let loginError = null;
  let pwInputRef;
  let modalRef;

  $: if ($auth.showLogin && pwInputRef) {
    setTimeout(() => pwInputRef?.focus(), 80);
  }

  // Lock background scrolling while the modal is open (iOS Safari otherwise
  // lets users swipe the page behind the fixed overlay).
  $: document.body.style.overflow = $auth.showLogin ? 'hidden' : '';
  onDestroy(() => {
    document.body.style.overflow = '';
  });

  function handleWindowKeydown(e) {
    if (!$auth.showLogin) return;

    if (e.key === 'Escape') {
      auth.setShowLogin(false);
      return;
    }

    if (e.key === 'Tab') trapFocus(e);
  }

  /// Keep keyboard focus inside the dialog while it is open.
  function trapFocus(e) {
    const root = modalRef;
    if (!root) return;
    const focusables = root.querySelectorAll('input:not([disabled]), button:not([disabled])');
    if (!focusables.length) return;

    const first = focusables[0];
    const last = focusables[focusables.length - 1];

    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  async function handleSubmit() {
    if (!password) return;
    loggingIn = true;
    loginError = null;
    const res = await login(password);
    if (res.success) {
      password = '';
    } else {
      loginError = res.error;
      // Bring the failed attempt back to the field so screen reader and
      // keyboard users can correct it immediately.
      setTimeout(() => {
        pwInputRef?.focus();
        pwInputRef?.select?.();
      }, 0);
    }
    loggingIn = false;
  }
</script>

<svelte:window on:keydown={handleWindowKeydown} />

{#if $auth.showLogin}
  <div class="modal-overlay">
    <div class="modal" role="dialog" aria-modal="true" aria-label="Anmeldung" bind:this={modalRef}>
      <div class="modal-head">
        <div class="modal-logo" aria-hidden="true">
          <svg style="width:26px;height:26px;" viewBox="0 0 24 24" fill="currentColor"><path d="M16 5v14L5 12z"/></svg>
        </div>
        <h2>Willkommen zurück</h2>
        <p>Bitte authentifiziere dich.</p>
      </div>
      <form class="modal-form" on:submit|preventDefault={handleSubmit}>
        <input
          bind:this={pwInputRef}
          type="password"
          bind:value={password}
          autocomplete="current-password"
          placeholder="Passwort"
          aria-label="Passwort"
          enterkeyhint="go"
        >
        <button type="submit" disabled={!password || loggingIn} class="login-btn">
          {#if !loggingIn}
            <span>Anmelden</span>
          {:else}
            <span><span class="spin sm"></span> Wird geladen...</span>
          {/if}
        </button>
        {#if loginError}
          <p class="login-error" role="alert">{loginError}</p>
        {/if}
      </form>
    </div>
  </div>
{/if}
