<script setup lang="ts">
import { computed, ref, watch } from "vue";
import SelectControl, { type SelectOption } from "./ui/select/SelectControl.vue";

const props = withDefaults(
  defineProps<{
    side: "old" | "new";
    /** The line the note starts on: the top of the range, which the composer can move. */
    lineStart: number;
    /** The line the `+` was on, and therefore the last line of the range. */
    lineEnd: number;
    /** The lines this note may start on, top of the file first, each marked as the diff marks it. */
    startOptions: SelectOption[];
    error?: string;
  }>(),
  { error: "" },
);
const emit = defineEmits<{
  submit: [content: string, lineStart: number];
  cancel: [];
  "update:lineStart": [lineStart: number];
}>();

const content = ref("");
const input = ref<HTMLTextAreaElement | null>(null);

/**
 * The first line of a range, which is never past the line the note ends on.
 *
 * Both ends are read from somewhere this component does not own: the parent holds the range on a
 * large diff, and on a small one they come from the diff library's own selection. Seeding from
 * either one unclamped is how a composer ends up showing `lines 30-10` and then saving a note
 * about one line.
 */
function clampedStart(value: number): number {
  return Math.min(Math.max(Math.trunc(value) || 1, 1), props.lineEnd);
}

const start = ref(clampedStart(props.lineStart));
const isRange = computed(() => start.value !== props.lineEnd);
const label = computed(() =>
  isRange.value ? `${props.side} lines ${start.value}-${props.lineEnd}` : `${props.side} line ${props.lineEnd}`,
);

// The range is the composer's while it is open, so a `+` clicked on a line above it moves the
// top of the range and the parent only mirrors what came out of here.
watch(
  () => props.lineStart,
  (value) => {
    start.value = clampedStart(value);
  },
);

// A new end is a different line to comment on, so the draft text goes with it. Widening the range
// instead keeps the text, which is the whole point of choosing the top of the range later.
watch(
  () => [props.side, props.lineEnd],
  async () => {
    content.value = "";
    // A selection that shrank can leave the top of the range past its new end.
    start.value = clampedStart(start.value);
    await Promise.resolve();
    input.value?.focus();
  },
  { immediate: true },
);

function setStart(value: number) {
  start.value = clampedStart(value);
  emit("update:lineStart", start.value);
}

function submit() {
  if (!content.value.trim()) return;
  emit("submit", content.value, start.value);
}
</script>

<template>
  <form
    class="m-1 border border-(--muster-border) bg-(--muster-bg-1) p-2 text-xs"
    aria-label="New review note"
    @submit.prevent="submit"
    @keydown.esc.stop.prevent="$emit('cancel')"
  >
    <!-- Where the note ends was decided by the +; where it starts is decided here, and it is
         offered from the moment the note opens rather than after a range exists. -->
    <p class="mb-1 flex items-center gap-1.5 font-mono text-[0.625rem] text-(--muster-text-faint)">
      <span class="shrink-0">{{ label }}</span>
      <SelectControl
        :model-value="String(start)"
        :options="startOptions"
        class="review-range-start"
        label="First line of the range"
        testid="range-start"
        @update:model-value="setStart(Number($event))"
      />
    </p>
    <textarea
      ref="input"
      v-model="content"
      rows="2"
      aria-label="Review note"
      placeholder="What should change here?"
      class="review-note-input w-full resize-y border border-(--muster-border) bg-(--muster-bg-0) px-2 py-1 font-mono text-xs text-(--muster-text) outline-none"
    />
    <p v-if="isRange" class="mt-1 text-[0.625rem] text-(--muster-text-faint)">
      This note covers the selected lines. Cancel to start over.
    </p>
    <p v-if="error" role="alert" class="mt-1 text-[0.625rem] text-(--muster-danger-fg)">{{ error }}</p>
    <div class="mt-1 flex items-center gap-2">
      <button type="submit" class="muster-button muster-button-tinted muster-button-xs" :disabled="!content.trim()">
        Save note
      </button>
      <button type="button" class="muster-button muster-button-ghost muster-button-xs" @click="$emit('cancel')">
        Cancel
      </button>
    </div>
  </form>
</template>

<style scoped>
/**
 * A line number rather than a field of its own: it steps the top of a range that is already drawn,
 * so it is sized to its digits and wears the composer's colours instead of the app's inputs, which
 * are sized for the forms people fill in.
 */
.review-range-start {
  min-height: 20px;
  max-width: 22ch;
  border-color: var(--muster-control-border-strong);
  padding: 0 6px;
  font-family: inherit;
  font-size: 0.625rem;
}
</style>
