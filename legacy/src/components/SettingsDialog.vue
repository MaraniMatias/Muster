<script setup lang="ts">
/* eslint-disable vue/html-self-closing */
/**
 * The preferences, drawn from the schema in `src/domain/settings.ts`.
 *
 * The form is generated rather than written out, so adding a preference is a field in the schema
 * and not a row of template: the dialog has no per-preference branch anywhere, which is the only
 * way it stays true as the list grows.
 *
 * Editing is against a draft, and nothing reaches the app or the file until Apply. That is what
 * makes Cancel possible at all, and it is also what keeps a half-chosen font size from being drawn
 * while it is being picked.
 */
import { computed, nextTick, ref, watch } from "vue";
import { ChevronRight as ChevronRightIcon } from "@lucide/vue";
import { ACKNOWLEDGEMENT, CREDITS, CREDITS_TITLE, REPOSITORY } from "../domain/credits";
import { formatBytes } from "../domain/files";
import type { ReviewFolder } from "../domain/files";
import { DEFAULT_SETTINGS, SETTINGS_SECTIONS, cloneSettings, valueAt, withValue } from "../domain/settings";
import type { AppSettings, ReviewStorageMode, SettingsField, SettingsPath, SettingsValue } from "../domain/settings";
import { SHORTCUT_GROUPS, shortcutChord } from "../domain/shortcuts";
import { trapDialogTab } from "../lib/dialog-focus";
import Button from "./ui/button/Button.vue";
import SelectControl from "./ui/select/SelectControl.vue";

const props = defineProps<{
  open: boolean;
  settings: AppSettings;
  saving: boolean;
  /**
   * The version of the build, or nothing while it is still being read or if it cannot be.
   *
   * It is a prop rather than something this dialog fetches because the window already knows what it
   * is, and a dialog that opened a second read of the same fact would be answering from its own
   * copy of it. Nothing here falls back to a version written by hand: a build that could not read
   * its own version shows no version rather than one that is nearly right.
   */
  version?: string | null;
  /**
   * Where exported reviews go for the mode the storage selector is showing, and what is in there.
   *
   * A prop rather than something this dialog fetches, for the same reason the version is: the row
   * describes a folder on disk, and a second read of it from here would be a second answer to the
   * same question. `null` is a folder that could not be read, which is drawn as nothing rather
   * than as a number that might be wrong.
   */
  reviewFolder?: ReviewFolder | null;
}>();

const emit = defineEmits<{
  close: [];
  /** The whole set, because the file holds the whole set and a partial save is not one. */
  apply: [settings: AppSettings];
  /** A web link to hand to the browser the machine has. The dialog opens no window of its own. */
  openExternalUrl: [url: string];
  /**
   * The storage mode the selector was moved to, before it is saved.
   *
   * The folder a mode writes to is on disk and is not in the file, so the row asks for it rather
   * than predicting it — which is what makes the selector answer where reviews go rather than only
   * naming two options.
   */
  reviewStorageChanged: [storage: ReviewStorageMode];
  clearReviewFolder: [storage: ReviewStorageMode];
}>();

const draft = ref<AppSettings>(props.settings);
const dialogElement = ref<HTMLElement | null>(null);
const cancelButton = ref<HTMLButtonElement | null>(null);

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    cancelButton.value?.focus();
  },
  { flush: "post" },
);

watch(
  () => props.saving,
  async (saving) => {
    if (!saving) return;
    await nextTick();
    dialogElement.value?.focus();
  },
);

/** Replaced whole on every open, so a draft left half-typed is never what the next open shows. */
watch(
  () => props.open,
  (open) => {
    if (open) draft.value = cloneSettings(props.settings);
  },
  { immediate: true },
);

// Asked on every move of the selector rather than on open only: the row under it names the folder
// the chosen mode writes to, and that folder is not in this file.
watch(
  () => draft.value.reviews.storage,
  (storage) => emit("reviewStorageChanged", storage),
);

const isDefault = computed(() => JSON.stringify(draft.value) === JSON.stringify(DEFAULT_SETTINGS));

function change(path: SettingsPath, value: SettingsValue) {
  draft.value = withValue(draft.value, path, value);
}

/**
 * The field's own range, applied as it is typed rather than at Apply: a font size of 400 is not a
 * preference, it is a number this app cannot draw, and saying so while the cursor is still in the
 * field is cheaper than after the window has been redrawn at it.
 */
function changeNumber(field: SettingsField & { kind: "number" }, input: HTMLInputElement) {
  const parsed = Number(input.value);
  if (!Number.isFinite(parsed)) return;
  if (field.path === "editor.indentation.size" && !Number.isInteger(parsed)) {
    input.value = String(valueAt(draft.value, field.path));
    return;
  }
  change(field.path, Math.min(field.limits.max, Math.max(field.limits.min, parsed)));
}

/** Every dot, not the first: `editor.indentation.size` has two, and a `for` that points at an id
 * nothing has is a label that does nothing when it is clicked. */
function controlId(path: SettingsPath) {
  return `settings-${path.replaceAll(".", "-")}`;
}

/** Reset replaces the draft rather than editing it, for the reason `withValue` copies. */
function resetDraft() {
  draft.value = cloneSettings(DEFAULT_SETTINGS);
}

/**
 * Whether this field holds something other than what the app ships.
 *
 * It is what the per-field reset is drawn on, and the reason that reset is drawn at all: a number,
 * a toggle and a select can all be put back by using them again, and a color cannot. A swatch has
 * no empty state, so a person who picked one has exactly two ways back to the default — type the
 * hex they are trying to undo — or a button. This is the button, and it only appears once the value
 * has actually moved, because a control that is always there is a control that means nothing.
 */
function isChanged(field: SettingsField) {
  return valueAt(draft.value, field.path) !== valueAt(DEFAULT_SETTINGS, field.path);
}

/** One field back to what the app ships. The whole set is the footer's Reset, and this is not it. */
function resetField(path: SettingsPath) {
  change(path, valueAt(DEFAULT_SETTINGS, path));
}

function requestClose() {
  if (!props.saving) emit("close");
}

function onDialogKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopPropagation();
    event.preventDefault();
    requestClose();
    return;
  }
  if (event.key !== "Tab") return;

  const dialog = dialogElement.value;
  if (!dialog) return;
  trapDialogTab(event, dialog);
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 grid place-items-center bg-(--muster-bg-0)/80 p-4"
    @click.self="requestClose"
  >
    <section
      ref="dialogElement"
      role="dialog"
      aria-modal="true"
      :aria-busy="saving"
      aria-labelledby="settings-title"
      tabindex="-1"
      class="surface-popover flex max-h-[85vh] w-full max-w-xl flex-col"
      @keydown="onDialogKeydown"
    >
      <header class="flex items-start justify-between gap-4 px-5 pt-5">
        <div>
          <h2 id="settings-title" class="text-base font-semibold text-(--muster-text)">Settings</h2>
          <p class="mt-1 text-xs text-(--muster-text-faint)">Saved to ~/.muster/config.yml</p>
        </div>
        <button type="button" aria-label="Close" :disabled="saving" class="muster-icon-control" @click="requestClose">
          ×
        </button>
      </header>

      <div class="min-h-0 flex-1 overflow-y-auto px-5 py-4">
        <section v-for="section in SETTINGS_SECTIONS" :key="section.id" class="mb-5 last:mb-0">
          <h3 class="mb-2 text-[0.6875rem] font-semibold tracking-wide text-(--muster-text-faint) uppercase">
            {{ section.title }}
          </h3>
          <div class="grid grid-cols-[1fr_8.5rem] items-center gap-x-4 gap-y-3">
            <!-- One pair per field, label then control, so a grid row is a field rather than a
                 column of labels beside a column of controls that have to be matched by eye. -->
            <template v-for="field in section.fields" :key="field.path">
              <label :for="controlId(field.path)">
                <span class="block text-xs text-(--muster-text)">{{ field.label }}</span>
                <span v-if="field.description" class="mt-0.5 block text-[0.6875rem] text-(--muster-text-faint)">
                  {{ field.description }}
                </span>
              </label>

              <div>
                <input
                  v-if="field.kind === 'toggle'"
                  :id="controlId(field.path)"
                  type="checkbox"
                  :disabled="saving"
                  class="muster-check"
                  :checked="valueAt(draft, field.path) === true"
                  @change="change(field.path, !valueAt(draft, field.path))"
                />

                <div v-else-if="field.kind === 'number'" class="flex items-center gap-2">
                  <input
                    :id="controlId(field.path)"
                    type="number"
                    :disabled="saving"
                    inputmode="numeric"
                    class="muster-input w-full"
                    :min="field.limits.min"
                    :max="field.limits.max"
                    :step="field.step ?? 1"
                    :value="valueAt(draft, field.path)"
                    @change="changeNumber(field, $event.target as HTMLInputElement)"
                  />
                  <span v-if="field.unit" class="w-6 shrink-0 text-[0.6875rem] text-(--muster-text-faint)">
                    {{ field.unit }}
                  </span>
                </div>

                <div v-else-if="field.kind === 'color'" class="flex items-center gap-1">
                  <input
                    :id="controlId(field.path)"
                    type="color"
                    :disabled="saving"
                    class="muster-input h-8 min-w-0 flex-1 cursor-pointer p-1"
                    :value="String(valueAt(draft, field.path))"
                    @input="change(field.path, ($event.target as HTMLInputElement).value)"
                  />
                  <!-- The swatch's own way back. It sits beside the control rather than replacing
                       it, so the color that is set stays on screen while it is being put back. -->
                  <Button
                    v-if="isChanged(field)"
                    variant="ghost"
                    size="sm"
                    :disabled="saving"
                    :aria-label="`Reset ${field.label} to default`"
                    @click="resetField(field.path)"
                  >
                    Reset
                  </Button>
                </div>

                <SelectControl
                  v-else
                  :id="controlId(field.path)"
                  :model-value="String(valueAt(draft, field.path))"
                  :options="field.options"
                  :disabled="saving"
                  variant="field"
                  :label="field.label"
                  @update:model-value="change(field.path, field.parse($event))"
                />
              </div>
            </template>
          </div>

          <!-- Where the exported notes go and what is in there. Drawn rather than generated from
               the schema for the reason Shortcuts is: the folder is on disk, its size moves, and
               the one action here deletes files in the person's home, so it is not a preference
               the schema could describe. It reads nothing itself — the window asks and passes the
               answer down, so there is one answer and this is where it is drawn. -->
          <div v-if="section.id === 'reviews'" class="mt-3" data-testid="review-folder">
            <!-- Nothing at all while the answer is not in: an empty row is the honest state for a
                 folder this window could not read, and a number it guessed would not be. -->
            <template v-if="reviewFolder && reviewFolder.storage === draft.reviews.storage">
              <p class="text-xs text-(--muster-text-secondary)">{{ reviewFolder.path }}</p>
              <p class="mt-1 text-[0.6875rem] text-(--muster-text-faint)">
                <template v-if="reviewFolder.storage === 'default'">
                  {{ reviewFolder.files }} {{ reviewFolder.files === 1 ? "note" : "notes" }},
                  {{ formatBytes(reviewFolder.bytes) }}.
                </template>
                <template v-else>Kept with the working directory, not in the app's folder.</template>
              </p>
              <!-- Only the app's own folder: notes inside a working directory belong to the
                   repository, so there is nothing here a delete could be honest about. -->
              <Button
                v-if="draft.reviews.storage === 'default' && reviewFolder.storage === 'default'"
                variant="ghost"
                size="sm"
                class="mt-2"
                :disabled="saving"
                :aria-label="`Delete ${reviewFolder.files === 1 ? 'the exported review note' : 'the exported review notes'} in ${reviewFolder.path}`"
                @click="emit('clearReviewFolder', draft.reviews.storage)"
              >
                Delete notes
              </Button>
            </template>
          </div>
        </section>

        <!-- Also a matter of record rather than of preference, for the same reason About is, and so
             it is drawn rather than generated from the schema: a chord has no value to write, and a
             control beside it would be a control that cannot do anything. `App.test.ts` reads this
             list and checks the window answers every chord in it. -->
        <section data-testid="shortcuts-section" class="border-t border-(--muster-border) pt-5">
          <h3 class="mb-2 text-[0.6875rem] font-semibold tracking-wide text-(--muster-text-faint) uppercase">
            Shortcuts
          </h3>
          <div v-for="group in SHORTCUT_GROUPS" :key="group.title" class="mb-3 last:mb-0">
            <p class="mb-1 text-[0.6875rem] text-(--muster-text-faint)">{{ group.title }}</p>
            <!-- One pair per chord, the way a field is one pair per preference: the keys on the left
                 and what they do on the right, so a chord is never read against another chord's
                 meaning. The chord is drawn as this platform presses it, because `⌘/` and `Ctrl+/` are
                 different keys and printing both of them would be printing one that does nothing. -->
            <dl class="grid grid-cols-[5.5rem_1fr] items-baseline gap-x-4 gap-y-1.5">
              <template v-for="shortcut in group.shortcuts" :key="shortcut.description">
                <dt>
                  <kbd class="muster-key">{{ shortcutChord(shortcut.keys) }}</kbd>
                </dt>
                <dd class="text-xs text-(--muster-text-secondary)">{{ shortcut.description }}</dd>
              </template>
            </dl>
          </div>
        </section>

        <!-- What the app is made of, which is a matter of record rather than of preference: no row
             here is written to the config file, and nothing about it is applied by the footer. The
             rule above it is what says so, because until then the whole dialog was about changing
             something and this is the one part of it that is only reading. -->
        <section data-testid="about-section" class="border-t border-(--muster-border) pt-5">
          <h3 class="mb-2 text-[0.6875rem] font-semibold tracking-wide text-(--muster-text-faint) uppercase">About</h3>

          <!-- What this build is. The version is the one fact a person opens this section for that
               nothing else on screen can tell them, and it is read from the binary rather than
               written here, so it cannot be a release behind the app it is in. It is drawn only once
               it is known: a half-read version is not a version. -->
          <p class="text-sm text-(--muster-text)" data-testid="about-version">
            {{ version ? `Muster v${version}` : "Muster" }}
          </p>

          <p class="mt-2 text-xs text-(--muster-text-secondary)">{{ ACKNOWLEDGEMENT }}</p>

          <!-- An `<a>`, not a button dressed as one: the address is text a person may want to
               select and copy, and this window's right-click menu is denied everywhere, so a link
               whose only way out is a click has no way out at all. `href` is here for that copy and
               for the focus ring; the navigation is prevented, because the webview is not a browser
               and following one would replace the window that has the drafts in it. -->
          <p class="mt-3 text-xs text-(--muster-text-faint)">
            Source and releases:
            <a
              :href="REPOSITORY"
              class="muster-link"
              data-testid="about-repository"
              @click.prevent="emit('openExternalUrl', REPOSITORY)"
            >
              {{ REPOSITORY }}
            </a>
          </p>

          <!-- The credits, folded. There are four groups and every entry carries a licence, which is
               longer than the whole rest of the section and is the answer to a question nobody came
               here with — so it stays one closed row, and the row says what is in it rather than
               inviting them to find out. The notices themselves are not here at all: the full
               licence texts ship as `THIRD-PARTY-NOTICES.txt` inside the bundle, which is where a
               notice has to travel to count as one. -->
          <details data-testid="about-credits" class="group mt-4">
            <summary
              class="flex cursor-pointer list-none items-center gap-1.5 text-xs text-(--muster-text-secondary) hover:text-(--muster-text)"
            >
              <ChevronRightIcon class="icon-xs transition-transform group-open:rotate-90" aria-hidden="true" />
              {{ CREDITS_TITLE }}
            </summary>

            <dl class="mt-3 space-y-3">
              <div v-for="group in CREDITS" :key="group.title">
                <dt class="text-[0.6875rem] text-(--muster-text-faint)">{{ group.title }}</dt>
                <dd class="mt-1 space-y-1">
                  <p v-for="entry in group.entries" :key="entry.name">
                    <span class="text-xs text-(--muster-text)">{{ entry.name }}</span>
                    <span class="text-(--muster-text-faint)"> — {{ entry.role }}, {{ entry.license }}</span>
                  </p>
                </dd>
              </div>
            </dl>
          </details>
        </section>
      </div>

      <footer class="flex items-center gap-2 border-t border-(--muster-border) px-5 py-4">
        <Button variant="ghost" :disabled="isDefault || saving" @click="resetDraft"> Reset to defaults </Button>
        <span class="flex-1" />
        <button
          ref="cancelButton"
          type="button"
          autofocus
          :disabled="saving"
          class="muster-button muster-button-subtle"
          @click="requestClose"
        >
          Cancel
        </button>
        <Button variant="tinted" :disabled="saving" @click="emit('apply', draft)">
          {{ saving ? "Saving…" : "Apply" }}
        </Button>
      </footer>
    </section>
  </div>
</template>

<style scoped>
.muster-check {
  margin-left: auto;
}

/*
 * A chord, drawn as the key it names: one key's worth of surface, the app's own face at the size
 * the rest of the dialog reads at, and a rule around it so `⌘` is not read as text. Two chords on one
 * row would wrap into each other, so the column they sit in is wide enough for the longest one.
 */
.muster-key {
  display: inline-block;
  border: 1px solid var(--muster-border);
  border-radius: 0.25rem;
  background: var(--muster-el);
  padding: 0.0625rem 0.375rem;
  color: var(--muster-text);
  font-family: var(--muster-font);
  font-size: 0.6875rem;
  line-height: 1.5;
  white-space: nowrap;
}
/*
 * A web link in the chrome. It keeps the colour the rest of the line is drawn in and is underlined
 * instead, because it is one address on a line of dim text rather than a thing among the app's own
 * controls, and the accent would say it was a control. The underline is what says it is clickable,
 * which matters more than usual here: the webview's right-click menu is denied on every surface, so
 * the address cannot be opened by right-click and copied from a context menu either.
 *
 * The focus ring is the stylesheet's, which already covers `a`.
 */
.muster-link {
  color: var(--muster-text-secondary);
  text-decoration: underline;
}
.muster-link:hover {
  color: var(--muster-text);
}
</style>
