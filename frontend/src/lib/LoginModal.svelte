<script>
  import { auth } from '../stores.js';
  import { login } from '../api.js';

  let password = '';
  let loggingIn = false;
  let loginError = null;
  let pwInputRef;

  $: if ($auth.showLogin && pwInputRef) {
    setTimeout(() => pwInputRef?.focus(), 80);
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
    }
    loggingIn = false;
  }
</script>

{#if $auth.showLogin}
  <div class="modal-overlay">
    <div class="modal" role="dialog" aria-modal="true" aria-label="Anmeldung">
      <div class="modal-head">
        <div class="modal-logo" aria-hidden="true">
          <svg style="width:26px;height:26px;" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 3c1.66 0 3 1.34 3 3s-1.34 3-3 3-3-1.34-3-3 1.34-3 3-3zm0 14.2c-2.5 0-4.71-1.28-6-3.22.03-1.99 4-3.08 6-3.08 1.99 0 5.97 1.09 6 3.08-1.29 1.94-3.5 3.22-6 3.22z"/></svg>
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
