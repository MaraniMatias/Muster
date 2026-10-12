import type { DocumentOrigin } from "./main-document";
import type { ReviewStorageMode } from "./settings";

/**
 * The folder exported reviews are kept in, as the settings dialog describes it.
 *
 * The path is absolute and is the folder exports are written to rather than one that had to
 * exist: the dialog asks where reviews go in order to change where they go, so it has to be able
 * to answer before anything has been written.
 */
export interface ReviewFolder {
  path: string;
  storage: ReviewStorageMode;
  files: number;
  bytes: number;
}

/** The exported file path and the storage origin the backend actually wrote to. */
export interface ReviewExport {
  path: string;
  origin: Exclude<DocumentOrigin, "checkout">;
}

/** What a clear removed. Reported rather than assumed: the app's own folder may hold other things. */
export interface ReviewFolderCleared {
  files: number;
  bytes: number;
}

/**
 * A byte count at the size a person reads it: whole bytes below a kilobyte, then one decimal and
 * the unit. "34 KB" answers whether a folder is getting out of hand; an exact figure does not.
 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

export type FileEntryKind = "file" | "directory" | "symlink";

export interface FileEntry {
  name: string;
  path: string;
  kind: FileEntryKind;
  /**
   * A Git checkout ignores this file. It is still listed, and the tree shows it a step quieter
   * so a build directory reads as present but uninteresting. Only `.git` is ever withheld.
   */
  ignored?: boolean;
}

export interface FileTree {
  entries: FileEntry[];
  truncated: boolean;
}

export interface FileContent {
  path: string;
  content: string;
}

/**
 * A path a terminal printed that this checkout holds and the preview can open, as the
 * checkout-relative spelling the rest of the app asks for. Absent rather than a flag, so nothing
 * can be opened that the probe did not confirm.
 */
export interface FileProbe {
  path: string;
}

/**
 * The Prettier options that govern a file, read from the nearest config above it. `null` is not
 * a checkout with nothing to say: it is a checkout that says Prettier's defaults apply.
 */
export interface PrettierConfig {
  /** File path relative to the config directory, for override matching. */
  path: string;
  options: Record<string, unknown>;
}

export interface CheckoutImage {
  mimeType: "image/png" | "image/jpeg" | "image/gif" | "image/webp";
  dataBase64: string;
  sizeBytes: number;
}

/**
 * The path a terminal would need to open this file: the checkout's own directory, then the path
 * the tree lists. The checkout directory is canonical and the file's path is relative, so this is
 * the one place the two are joined, and it owns the separator: a checkout that arrives with a
 * trailing slash must not produce a doubled one.
 */
export function absoluteFilePath(
  canonicalPath: string,
  path: string,
  origin: DocumentOrigin = "checkout",
  reviewRoot?: string,
): string {
  const root = origin === "checkout" ? canonicalPath : reviewRoot;
  if (!root) throw new Error("review file path requires its root");
  return `${root.replace(/[\\/]+$/, "")}/${path}`;
}
