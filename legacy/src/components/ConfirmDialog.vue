<script setup lang="ts">
/* The overlay and the surface are the ones `WorktreeDialog` draws, so a question asked here
   looks like the rest of the app rather than like the browser's own dialog. */
import { computed } from "vue";
import { handleDialogKeydown } from "../lib/dialog-focus";

const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    /** The body of the question: what the action does, and what it leaves alone. */
    message: string;
    confirmLabel: string;
    /** Fills the confirming button in the failure colour, for the answers that are not neutral. */
    destructive?: boolean;
    busy?: boolean;
  }>(),
  { destructive: false, busy: false },
);

const emit = defineEmits<{ confirm: []; close: [] }>();

/** The heading names the dialog, so it is what a screen reader announces on open. */
const titleId = computed(() => `confirm-${props.title.replace(/\W+/g, "-").toLowerCase()}`);

function onDialogKeydown(event: KeyboardEvent) {
  handleDialogKeydown(event, () => emit("close"));
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 grid place-items-center bg-(--muster-bg-0)/80 p-4"
    @click.self="emit('close')"
  >
    <section
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      :aria-labelledby="titleId"
      class="surface-popover w-full max-w-md p-5"
      @keydown="onDialogKeydown"
    >
      <h2 :id="titleId" class="text-base font-semibold text-(--muster-text)">{{ title }}</h2>
      <p class="mt-2 text-sm text-(--muster-text-secondary)">{{ message }}</p>
      <footer class="mt-5 flex justify-end gap-2">
        <!-- Focus opens on the answer that does nothing, so a stray Enter cannot answer yes. -->
        <button type="button" autofocus class="muster-button muster-button-subtle" @click="emit('close')">
          Cancel
        </button>
        <button
          type="button"
          :disabled="busy"
          :class="destructive ? 'muster-button-danger' : 'muster-button muster-button-tinted'"
          @click="emit('confirm')"
        >
          {{ busy ? "Working…" : confirmLabel }}
        </button>
      </footer>
    </section>
  </div>
</template>
