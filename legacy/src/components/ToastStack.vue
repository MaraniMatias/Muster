<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { X as XIcon } from "@lucide/vue";
import { useToasts } from "../presentation/toasts";

const { toasts, dismiss, dismissNewest, hold, release } = useToasts();

/** A field owns its own Escape, so a toast never takes one away from what is being typed. */
function isEditableTarget(target: EventTarget | null) {
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  );
}

function onKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape" || isEditableTarget(event.target)) return;
  dismissNewest();
}

onMounted(() => window.addEventListener("keydown", onKeydown));
onUnmounted(() => window.removeEventListener("keydown", onKeydown));
</script>

<template>
  <!-- The stack is chrome, not content: it never takes the focus, and the empty space around
       the toasts stays click-through so a toast cannot swallow a click on the panel. -->
  <div v-if="toasts.length" class="toast-stack">
    <div
      v-for="toast in toasts"
      :key="toast.id"
      class="toast"
      :class="`toast-${toast.level}`"
      :role="toast.level === 'error' ? 'alert' : 'status'"
      @mouseenter="hold(toast.id)"
      @mouseleave="release(toast.id)"
    >
      <p class="toast-message">{{ toast.message }}</p>
      <button type="button" class="toast-dismiss" :aria-label="`Dismiss: ${toast.message}`" @click="dismiss(toast.id)">
        <XIcon class="icon-xs" aria-hidden="true" />
      </button>
    </div>
  </div>
</template>

<style scoped>
/* Bottom center over the main panel. Newest last in the DOM and first at the bottom, so
   every toast pushes the ones above it up instead of covering them. Above the drawer and the
   splitter handles, below the worktree dialog's scrim: a modal ask comes first. */
.toast-stack {
  position: fixed;
  z-index: 40;
  bottom: 1rem;
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  flex-direction: column-reverse;
  align-items: center;
  gap: 6px;
  width: min(420px, calc(100vw - 2rem));
  pointer-events: none;
}

/* The message is centred in the toast and the dismiss button keeps its corner, which takes two
   columns and not one: a 20px column on each side, the right one for the button and the left one
   empty. Centring the text in the space the button leaves it would sit it a whole button-width to
   the left of the middle, which is the one thing centring it here is meant not to do. */
.toast {
  display: grid;
  grid-template-columns: 20px minmax(0, 1fr) 20px;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 8px;
  border: 1px solid var(--muster-border-strong);
  /* How loud a message is is a stripe down the left edge, opposite the dismiss button: a grey
     note, a red failure. The edge states its own importance without putting a glyph in the text's
     way. */
  border-left: 4px solid var(--muster-text-secondary);
  border-radius: 0;
  background: var(--muster-bg-2);
  box-shadow: var(--muster-shadow);
  color: var(--muster-text);
  font-size: 0.8125rem;
  line-height: 1.4;
  pointer-events: auto;
}

.toast-error {
  border-left-color: var(--muster-danger);
}

.toast-message {
  grid-column: 2;
  margin: 0;
  overflow-wrap: anywhere;
  text-align: center;
}

.toast-dismiss {
  grid-column: 3;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  flex-shrink: 0;
  padding: 0;
  border: none;
  border-radius: 0;
  background: transparent;
  color: var(--muster-text-secondary);
  cursor: pointer;
}

.toast-dismiss:hover {
  background: var(--muster-control-hover);
  color: var(--muster-text);
}
</style>
