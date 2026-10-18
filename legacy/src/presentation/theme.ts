import { ref, type Ref } from "vue";

/**
 * The two palettes the app is drawn in, named the way the `ui.theme` preference names them.
 *
 * `src/muster.css` holds the colors themselves; what this holds is which of the two the window is
 * in. Everything painted by a stylesheet follows the `data-theme` attribute the shell writes and
 * needs nothing from here. This is for the two things CSS cannot reach: the terminal, whose theme
 * is an object rather than a declaration, and the diff library, which is handed a theme name.
 */
export type Theme = "light" | "dark";

/** Which palette is on screen. A module singleton because there is one window and one of each. */
export const theme: Ref<Theme> = ref("dark");
