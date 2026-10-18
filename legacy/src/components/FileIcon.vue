<script setup lang="ts">
import { computed } from "vue";
import {
  CATPPUCCIN_DIRECTORY_ICON,
  CATPPUCCIN_ICON_MARKUP,
  CATPPUCCIN_ICON_VIEWBOX,
  CATPPUCCIN_ICON_VIEWBOX_OVERRIDE,
  CATPPUCCIN_NAMED_DIRECTORY,
  catppuccinIconFor,
} from "../lib/catppuccin-icons";
import type { FileEntryKind } from "../domain/files";

const props = withDefaults(
  defineProps<{
    name: string;
    kind: FileEntryKind;
    /** Whether the row draws itself quieter, and how far: a dotfile, or a Git-ignored file. */
    prominence?: "normal" | "hidden" | "ignored";
  }>(),
  { prominence: "normal" },
);

const icon = computed(() =>
  props.kind === "directory"
    ? (CATPPUCCIN_NAMED_DIRECTORY[props.name] ?? CATPPUCCIN_DIRECTORY_ICON.collapsed)
    : catppuccinIconFor(props.name),
);

const viewBox = computed(() => CATPPUCCIN_ICON_VIEWBOX_OVERRIDE[icon.value] ?? CATPPUCCIN_ICON_VIEWBOX);
</script>

<template>
  <!-- The upstream markup is inlined rather than loaded as an image: nothing is fetched, and
       the content security policy's `img-src` never enters into it. The strokes carry their own
       per-type colour, so quieting a row is opacity on this element. The markup interpolated below is
       one of our own literal SVG strings, so it is never user input. -->
  <!-- eslint-disable vue/no-v-html -->
  <svg
    class="file-icon shrink-0"
    :class="`file-icon-${prominence}`"
    :viewBox="viewBox"
    width="14"
    height="14"
    aria-hidden="true"
    v-html="CATPPUCCIN_ICON_MARKUP[icon]"
  />
  <!-- eslint-enable vue/no-v-html -->
</template>

<style scoped>
.file-icon {
  display: block;
  overflow: visible;
}

/* Three steps of prominence, all still readable: a dotfile steps down, and a file Git ignores
   steps down again. The numbers are the whole ramp, so they are the only thing to tune. */
.file-icon-hidden {
  opacity: 0.72;
}
.file-icon-ignored {
  opacity: 0.48;
}
</style>
