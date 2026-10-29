<script lang="ts">
  import ChatApp from "$lib/ChatApp.svelte";
  import { page } from "$app/state";

  let { children } = $props();
</script>

<!--
  Single ChatApp instance for the whole app: route changes only update the
  sessionId prop (adopted by ChatApp's appliedSession effect). Mounting one
  ChatApp per route unmounted all state on every navigation — deleting the
  active chat blanked the entire sidebar until initApp reloaded, and any
  in-flight stream was aborted by onDestroy.
-->
<ChatApp sessionId={page.params.id ?? null} />
{@render children()}
