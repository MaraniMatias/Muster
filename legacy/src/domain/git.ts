export interface GitChangedFile {
  path: string;
  oldPath?: string;
  status: string;
  /** Absent rather than zero when Git has no line count for the file: `--numstat` prints `-`
   *  for both columns of a file it cannot count, such as a binary one. */
  additions?: number;
  deletions?: number;
}

/** The lines a change set adds and removes. */
export interface GitDiffStats {
  additions: number;
  deletions: number;
}

/** The totals of every registered Git checkout, keyed by checkout id. One call covers the
 *  whole sidebar, so it does not have to ask checkout by checkout. */
export type GitCheckoutDiffStats = Record<string, GitDiffStats>;

/** The changed files of one checkout, each with the lines it adds and removes. */
export type GitFileDiffStats = GitChangedFile[];

/** One checkout's file activity, with the paths a batch moved inside it. */
export interface CheckoutFileActivity {
  checkoutId: string;
  /** Relative to the checkout, which is the form a document's relative references resolve to.
   *  Empty means the batch was too large to carry what it moved, not that nothing moved. */
  paths: string[];
}

export interface GitStatus {
  branch?: string;
  head?: string;
  defaultBranch: string;
  aheadCount: number;
  files: GitChangedFile[];
}

export interface GitFileDiff {
  path: string;
  patch: string;
  /** What this diff is of, hashed by the backend from the lines it read. It is what two answers are
   *  compared on rather than the patch: a large, a too large and a binary diff all carry no patch at
   *  all, so on the patch alone any two of them are the same diff and the view behind them keeps
   *  serving the pages of the first. Stable when the diff has not moved, which is what keeps a
   *  refresh of an unmoved diff from rebuilding the reading around the open note composer. */
  revision: string;
  /** The whole text of each side of the diff, which is what a grammar reads rather than the
   *  patch's hunks: a hunk inside `<script setup lang="ts">` is markup to a grammar that never saw
   *  the opening tag. Absent when the side has no text: a binary or symlink diff, a diff too large
   *  to hold, an added file's old side, a removed file's new one, or a file past the backend's cap. */
  oldContent?: string;
  newContent?: string;
  isBinary: boolean;
  large: boolean;
  tooLarge: boolean;
  totalLines: number;
  hunks: GitDiffHunk[];
  symlinkTarget?: string;
}

export interface GitDiffHunk {
  startLine: number;
  endLine: number;
  title: string;
}

export type GitDiffLineKind = "hunk" | "added" | "removed" | "context" | "meta";

export interface GitDiffPageLine {
  index: number;
  kind: GitDiffLineKind;
  text: string;
  oldLineNumber: number | null;
  newLineNumber: number | null;
}

export interface GitDiffPage {
  path: string;
  startLine: number;
  totalLines: number;
  lines: GitDiffPageLine[];
}
