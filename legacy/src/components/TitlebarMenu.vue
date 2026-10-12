<script setup lang="ts">
import type { Component } from "vue";
import { computed, ref } from "vue";
import {
  DropdownMenuContent,
  DropdownMenuFilter,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "reka-ui";
import { Check as CheckIcon } from "@lucide/vue";
import type { TitlebarMenuItem, TitlebarMenuSection } from "../domain/titlebar-menu";
import { visibleSections } from "../domain/titlebar-menu";

const props = withDefaults(
  defineProps<{
    /** What the crumb says. It is the whole name too, so a truncated crumb can be read. */
    label: string;
    sections: TitlebarMenuSection[];
    /**
     * The crumb menus share one open state in the shell, so opening one closes the others.
     * The component does not decide that on its own: it reports and obeys.
     */
    open: boolean;
    /** When set, the menu opens on a search field over the rows. */
    searchPlaceholder?: string;
    align?: "start" | "end";
    /**
     * The glyph that stands for what the crumb names, drawn ahead of the name.
     *
     * It is the one the sidebar gives the same row, so the line reads as that panel sideways
     * instead of as a row of text that happens to have one icon in it.
     */
    icon?: Component;
    /** Where this crumb sits in the line, which is what says how bright it reads and what it
     *  gives up when the line runs out of room. */
    crumb?: "workdir" | "branch" | "item";
    testid?: string;
  }>(),
  { searchPlaceholder: undefined, align: "start", icon: undefined, crumb: "item", testid: undefined },
);

const emit = defineEmits<{ "update:open": [boolean] }>();

const query = ref("");
const sections = computed(() => visibleSections(props.sections, query.value));
/**
 * What the search emptied, to say so. A menu that was never a list (one that only ever had a
 * note and an action) has nothing to report, and the actions at the foot never count as a hit.
 */
const nothingMatched = computed(
  () =>
    !!props.searchPlaceholder &&
    !sections.value.some((section) => section.kind !== "separator" && section.items.some((item) => !item.pinned)),
);

function rows(section: TitlebarMenuSection): TitlebarMenuItem[] {
  return section.kind === "separator" ? [] : section.items;
}

/**
 * A key that survives the filter. While the user types, sections appear and disappear, so an
 * index would hand one section's DOM node (and its focus and highlight) to another's content.
 * A group is named by its label; a separator has nothing to be named by and keeps its position.
 */
function sectionKey(section: TitlebarMenuSection, index: number): string {
  return section.kind === "group" ? `group-${section.label}` : `${section.kind}-${index}`;
}

/** Closing forgets what was typed, so the menu opens on the whole list again. */
function onOpenChange(open: boolean) {
  if (!open) query.value = "";
  emit("update:open", open);
}
</script>

<template>
  <DropdownMenuRoot :open="open" @update:open="onOpenChange">
    <DropdownMenuTrigger
      class="text-menu-control select-none"
      :class="[`crumb-${crumb}`, crumb === 'item' && 'crumb-terminal']"
      :data-testid="testid"
      :title="label"
    >
      <component :is="icon" v-if="icon" class="icon-xs crumb-icon" aria-hidden="true" />
      <span class="truncate">{{ label }}</span>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent
        side="bottom"
        :align="align"
        :side-offset="4"
        class="surface-popover muster-menu muster-menu-scroll w-72"
      >
        <DropdownMenuFilter
          v-if="searchPlaceholder"
          v-model="query"
          :placeholder="searchPlaceholder"
          :auto-focus="true"
          class="muster-menu-search"
        />
        <template v-for="(section, index) in sections" :key="sectionKey(section, index)">
          <DropdownMenuSeparator v-if="section.kind === 'separator'" class="menu-separator" />
          <!-- A group is a real group: its label names it for a screen reader, which would
               otherwise walk the rows without knowing which set they belong to. -->
          <DropdownMenuGroup v-else>
            <DropdownMenuLabel v-if="section.kind === 'group'" class="group-header" :title="section.label">
              {{ section.label }}
            </DropdownMenuLabel>
            <DropdownMenuItem
              v-for="item in rows(section)"
              :key="item.id"
              class="menu-item select-none"
              :class="{ 'is-pinned': item.pinned }"
              :disabled="item.disabled"
              :title="item.title ?? item.label"
              :data-testid="`menu-item-${item.id}`"
              :role="item.choice ? 'menuitemradio' : undefined"
              :aria-checked="item.choice ? item.checked === true : undefined"
              @select="item.run()"
            >
              <span class="menu-item-label">{{ item.label }}</span>
              <!-- On a set of alternatives the check is the only thing saying which one, and a
                   screen reader is told so through the row's role rather than through this glyph:
                   every row of the set carries `aria-checked`, the ones that are not current included,
                   because a radio group that only reports the selected item leaves the reader to
                   infer the size of the set. A check on a row that only names what is open is a
                   different thing, so the glyph there is hidden from it and nothing else changes. -->
              <CheckIcon v-if="item.checked" class="icon-xxs menu-check" aria-hidden="true" />
              <span v-if="item.hint" class="menu-item-hint">{{ item.hint }}</span>
            </DropdownMenuItem>
          </DropdownMenuGroup>
        </template>
        <!-- A status, so that a filter which empties the list is announced and not only drawn. -->
        <p v-if="nothingMatched" class="menu-note" role="status">No matches</p>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
