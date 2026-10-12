<script setup lang="ts">
import { ref } from "vue";
import { anchorOutcome, reviewLineRange } from "../domain/review";
import type { AnchorOutcome, ReviewNote } from "../domain/review";

const props = defineProps<{
  notes: ReviewNote[];
  /** What the diff can prove about each anchor; absent means "not judged". */
  outcomes?: Map<string, AnchorOutcome>;
}>();
const emit = defineEmits<{
  updateNote: [id: string, content: string];
  deleteNote: [id: string];
  clearOutdated: [id: string];
  resolveNote: [id: string];
}>();

const editingId = ref<string | null>(null);
const draft = ref("");

function startEditing(note: ReviewNote) {
  editingId.value = note.id;
  draft.value = note.content;
}

function save(id: string) {
  if (!draft.value.trim()) return;
  emit("updateNote", id, draft.value);
  editingId.value = null;
}

/** The verdict for a note the agent has already seen, or null when it is not judged. */
function verdictFor(note: ReviewNote) {
  const outcome = props.outcomes?.get(note.id);
  return outcome ? anchorOutcome(outcome) : null;
}

/**
 * Resolving is offered only when the diff can back it. Without a verdict the note predates
 * this render, and offering the button would let the user mark something resolved that
 * nothing has actually checked.
 */
function canResolve(note: ReviewNote): boolean {
  return verdictFor(note)?.resolvable === true;
}
</script>

<template>
  <ul class="m-0 list-none space-y-1 p-1">
    <!-- Semantic status colors stay on the labels; the card only carries its border. -->
    <li
      v-for="note in props.notes"
      :key="note.id"
      class="border border-(--muster-border) bg-(--muster-bg-1) px-2 py-1 text-xs"
    >
      <p class="flex items-center gap-2 font-mono text-[0.625rem] text-(--muster-text-faint)">
        <span>{{ note.side }}:{{ reviewLineRange(note) }}</span>
        <span v-if="note.status === 'sent'" class="text-(--muster-text-secondary)">sent</span>
        <span v-if="note.status === 'resolved'" class="text-(--muster-success-fg)">resolved</span>
        <span v-if="note.outdated" class="text-(--muster-warning-fg)">outdated</span>
        <span class="ml-auto flex gap-2">
          <button
            v-if="note.status === 'sent' && !note.outdated && canResolve(note)"
            type="button"
            class="muster-button muster-button-ghost muster-button-xs"
            :aria-label="`Mark note on line ${note.lineStart} as resolved`"
            @click="$emit('resolveNote', note.id)"
          >
            Resolved
          </button>
          <button
            v-if="note.outdated"
            type="button"
            class="muster-button muster-button-ghost muster-button-xs"
            :aria-label="`Accept note on line ${note.lineStart} even though the line changed`"
            @click="$emit('clearOutdated', note.id)"
          >
            Accept
          </button>
          <button
            type="button"
            class="muster-button muster-button-ghost muster-button-xs"
            :aria-label="`Edit note on line ${note.lineStart}`"
            @click="startEditing(note)"
          >
            Edit
          </button>
          <button
            type="button"
            class="muster-button muster-button-ghost muster-button-xs review-note-delete"
            :aria-label="`Delete note on line ${note.lineStart}`"
            @click="$emit('deleteNote', note.id)"
          >
            Delete
          </button>
        </span>
      </p>
      <form v-if="editingId === note.id" aria-label="Edit review note" @submit.prevent="save(note.id)">
        <textarea
          v-model="draft"
          rows="2"
          aria-label="Review note"
          class="review-note-input mt-1 w-full resize-y border border-(--muster-border) bg-(--muster-bg-0) px-2 py-1 font-mono text-xs text-(--muster-text) outline-none"
          @keydown.esc.stop.prevent="editingId = null"
        />
        <div class="mt-1 flex gap-2">
          <button type="submit" class="muster-button muster-button-tinted muster-button-xs">Save</button>
          <button type="button" class="muster-button muster-button-ghost muster-button-xs" @click="editingId = null">
            Cancel
          </button>
        </div>
      </form>
      <p v-else class="mt-0.5 whitespace-pre-wrap text-(--muster-text)">{{ note.content }}</p>
      <p v-if="note.outdated" class="mt-0.5 text-[0.625rem] text-(--muster-warning-fg)">
        The line this note points at changed. It is kept out of the review sent to the agent.
      </p>
      <p
        v-else-if="note.status === 'sent' && verdictFor(note)"
        class="mt-0.5 text-[0.625rem]"
        :class="verdictFor(note)!.resolvable ? 'text-(--muster-text-dim)' : 'text-(--muster-text-faint)'"
      >
        {{ verdictFor(note)!.message }}
      </p>
    </li>
  </ul>
</template>

<style scoped>
.review-note-delete:hover:not(:disabled),
.review-note-delete:focus-visible {
  color: var(--muster-danger-fg);
}
</style>
