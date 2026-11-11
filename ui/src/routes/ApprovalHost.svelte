<script lang="ts">
  import type { ApprovalAnswer } from "../lib/api";
  import type { ApprovalRequest } from "../lib/types";
  import ApprovalDialog from "./ApprovalDialog.svelte";

  let {
    requests,
    titleFor,
    onanswer,
  }: {
    requests: ApprovalRequest[];
    titleFor: (conversationId: number) => string;
    onanswer: (id: string, a: ApprovalAnswer) => void;
  } = $props();

  const current = $derived(requests[0] ?? null);
</script>

{#if current}
  <!-- A fresh dialog per request: no state (or click) carries over to the next one. -->
  {#key current.id}
    <ApprovalDialog request={current} conversationTitle={titleFor(current.conversation_id)} {onanswer} />
  {/key}
{/if}
