/**
 * What Muster is built out of, and the licence each piece travels under.
 *
 * These are not preferences, which is why they are not in `SETTINGS_SECTIONS`: nothing here is
 * written to `~/.muster/config.yml`, nothing is restored, and changing your mind about it is not
 * what anybody means to do. They are a record of fact about the build, so they live in their own
 * module and the About section draws them.
 *
 * Every licence below requires its notice to travel with the software, and until this existed the
 * notices lived only in the source: the Catppuccin copyright in the header of a generated `.ts`,
 * the Fira Code OFL beside the fonts in a directory Vite never bundles, the Zed palette credit in
 * a comment above a `:root` block. A person with the app installed had none of them.
 *
 * No versions. A version written by hand is wrong the first time a dependency is bumped and nothing
 * here would say so; `pnpm-lock.yaml` is the record of what this build actually links against.
 */
export interface CreditsEntry {
  /** The name the project ships under, so a person reading this can find it. */
  name: string;
  /** What it does for Muster, in a line: a list of names says nothing about why any of them is here. */
  role: string;
  /** The licence, spelled the way the project spells it, because that is what the notice has to say. */
  license: string;
  /**
   * The package or crate the entry is, when there is one to name.
   *
   * An attribution whose subject has been swapped out is a lie, and nothing in the app can see it:
   * the credit is text and the dependency is a key in a manifest. Naming it here is what lets
   * `credits.test.ts` read `package.json` and fail when the two stop agreeing.
   */
  package?: string;
}

export interface CreditsGroup {
  title: string;
  entries: CreditsEntry[];
}

/**
 * Copied artwork rather than code: the drawings and the colours are in the binary, not fetched at
 * runtime, so their licences are conditions of shipping them rather than a formality.
 */
const ARTWORK: CreditsGroup = {
  title: "Artwork",
  entries: [
    {
      name: "Catppuccin",
      role: "the file-type icons in the sidebar, the tree and the file header",
      license: "MIT",
    },
    {
      name: "Lucide",
      role: "the icons in the interface itself",
      license: "ISC",
      package: "@lucide/vue",
    },
  ],
};

/** The two palettes, taken from Zed's One themes rather than invented here. */
const COLORS: CreditsGroup = {
  title: "Colors",
  entries: [
    {
      name: "Zed",
      role: "One Dark and One Light, retained for editor, diff, and terminal content",
      license: "MIT",
    },
  ],
};

/** One face for the terminal and the editor, bundled so the two never disagree. */
const TYPE: CreditsGroup = {
  title: "Type",
  entries: [
    {
      name: "Fira Code Nerd Font Mono",
      role: "the bundled typeface, in the terminal and in the editor",
      license: "SIL Open Font License 1.1",
    },
  ],
};

/**
 * The libraries the app is assembled from, named by what they do here rather than by what they are.
 *
 * Every entry that can be named as a package is, and `credits.test.ts` checks each of those against
 * `package.json`. A dependency swapped for another fails that test, which is the point: this list
 * is the one place that claims what shipped.
 */
const LIBRARIES: CreditsGroup = {
  title: "Built with",
  entries: [
    { name: "Vue", role: "the interface", license: "MIT", package: "vue" },
    { name: "CodeMirror", role: "the editor and its language modes", license: "MIT", package: "@codemirror/view" },
    { name: "xterm.js", role: "the terminal", license: "MIT", package: "@xterm/xterm" },
    { name: "Shiki", role: "syntax highlighting in the read-only preview", license: "MIT", package: "shiki" },
    { name: "markdown-it", role: "Markdown rendering", license: "MIT", package: "markdown-it" },
    {
      name: "DOMPurify",
      role: "sanitizing the HTML Markdown can carry",
      license: "MPL-2.0 OR Apache-2.0",
      package: "dompurify",
    },
    { name: "Reka UI", role: "the menus, popovers and splitters", license: "MIT", package: "reka-ui" },
    {
      name: "@git-diff-view",
      role: "drawing the diffs",
      license: "MIT",
      package: "@git-diff-view/vue",
    },
    { name: "Lezer", role: "syntax highlighting in the editor", license: "MIT", package: "@lezer/highlight" },
    {
      name: "Tauri",
      role: "the window this runs in and the Rust behind it",
      license: "Apache-2.0 OR MIT",
      package: "@tauri-apps/api",
    },
  ],
};

export const CREDITS: CreditsGroup[] = [ARTWORK, COLORS, TYPE, LIBRARIES];

/**
 * The line above the list, said once instead of once per entry.
 *
 * Every project below is under a licence that allows this use, and the licence beside its name is
 * the notice that licence asks to travel with it. The licences themselves are not reproduced here
 * because a list of them is longer than the About section and reads as nothing; the notice is the
 * name and the terms, and the full text is a `LICENSE` file away in every one of those projects.
 *
 * It names the disclosure rather than the list because that is where the list is: a person reading
 * this has the credits folded away under them, and a line saying they are below is a claim about
 * what is on screen.
 */
export const ACKNOWLEDGEMENT =
  "Muster is MIT licensed. Most of what is drawn in it is not ours: the icons, the colors, the " +
  "typeface and the libraries all belong to the projects named in the credits, each under a " +
  "licence that allows this use. Marani Matias Ezequiel built the rest.";

/**
 * What the closed disclosure is called, which is the only name the list has when it is not drawn.
 *
 * A closed `<details>` shows its summary and nothing else, so this is the whole of what a person
 * knows about the licences travelling with the app until they open it. It says what is inside
 * rather than inviting them to look, because a row that only says "Show more" is a row whose
 * contents nobody opens.
 */
export const CREDITS_TITLE = "Credits and licences";

/**
 * Where the source is, which the About section draws as a link.
 *
 * The address is drawn as text inside an `<a>` rather than as a button, so a person who wants to
 * send it to someone can select and copy it: this window's right-click menu is denied on every
 * surface, so there is no "Copy Link Address" to fall back on. It is also the same address the
 * macOS About panel carries as a link, so the two agree about where this is.
 */
export const REPOSITORY = "https://github.com/MaraniMatias/Muster";
