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
