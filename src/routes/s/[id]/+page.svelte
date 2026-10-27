<script lang="ts">
  import ChatApp from "$lib/ChatApp.svelte";
  import { page } from "$app/state";
</script>

<ChatApp sessionId={page.params.id ?? null} />
