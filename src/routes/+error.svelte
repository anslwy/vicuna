<script lang="ts">
  // The whole UI lives in +layout.svelte (one ChatApp), so a routing error
  // has nothing useful to show. A 404 is a window opened on a path the SPA
  // has no route for (e.g. `index.html?server=…` from open_server_window):
  // go to `/` at once, keeping the query, while the launch splash covers it.
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";

  onMount(() => {
    if (page.status === 404) void goto(`/${window.location.search}`, { replaceState: true });
  });
</script>

{#if page.status !== 404}
  <p class="route-error">{page.status} {page.error?.message ?? ""}</p>
{/if}

<style>
  .route-error { position: fixed; bottom: 12px; left: 50%; transform: translateX(-50%); font-size: 12px; color: #9a9a9a; margin: 0; }
</style>
