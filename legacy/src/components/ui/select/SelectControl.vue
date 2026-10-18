<script setup lang="ts">
/**
 * A list of choices, drawn by the app.
 *
 * A native `<select>` paints its own popup in the system colours, and on Linux that popup
 * arrives unstyled (no surface, no check, no app type) while the control itself is the one
 * field the stylesheet cannot reach. Reka gives the list, the roles and the keyboard; the
 * classes give it the app's face, so the same thing looks the same on every platform.
 */
import { Check as CheckIcon, ChevronDown as ChevronDownIcon } from "@lucide/vue";
import {
  SelectContent,
  SelectItem,
  SelectItemIndicator,
  SelectItemText,
  SelectPortal,
  SelectRoot,
  SelectTrigger,
  SelectValue,
  SelectViewport,
} from "reka-ui";

export type SelectOption = { value: string; label: string };

// The root is renderless, so a class or a testid passed in from outside would land nowhere;
// they belong to the trigger, which is the only element the component actually draws.
defineOptions({ inheritAttrs: false });

defineProps<{
  options: SelectOption[];
  /** Names the control for a screen reader, because the trigger has no visible label of its own. */
  label?: string;
  /** `field` matches the inputs around it in a dialog; without it the trigger is a toolbar control. */
  variant?: "control" | "field";
  testid?: string;
}>();

/** Required, because a select with nothing chosen has nothing to say: the caller picks the first. */
const model = defineModel<string>({ required: true });
</script>

<template>
  <SelectRoot v-model="model">
    <SelectTrigger
      v-bind="$attrs"
      class="muster-select"
      :class="{ 'muster-select-field': variant === 'field' }"
      :aria-label="label"
      :data-testid="testid"
    >
      <SelectValue />
      <ChevronDownIcon class="icon-xs shrink-0 text-(--muster-text-faint)" aria-hidden="true" />
    </SelectTrigger>
    <!-- Portalled like every other list in the app: the rows are absolutely positioned and the
         pane under them paints over anything left in place. -->
    <SelectPortal>
      <SelectContent
        class="surface-popover muster-menu muster-select-content"
        :aria-label="label"
        :side-offset="4"
        position="popper"
      >
        <SelectViewport>
          <SelectItem v-for="option in options" :key="option.value" :value="option.value" class="menu-item">
            <SelectItemText>{{ option.label }}</SelectItemText>
            <SelectItemIndicator>
              <CheckIcon class="icon-xxs menu-check" aria-hidden="true" />
            </SelectItemIndicator>
          </SelectItem>
        </SelectViewport>
      </SelectContent>
    </SelectPortal>
  </SelectRoot>
</template>
