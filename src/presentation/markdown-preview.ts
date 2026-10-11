import { onBeforeUnmount, ref } from "vue";
import { readCheckoutMarkdownImage } from "../lib/ipc";
import type { CheckoutImage } from "../domain/files";
import type { DocumentOrigin } from "../domain/main-document";
import type { MarkdownImageReference } from "../lib/markdown-preview";

const MAX_MARKDOWN_IMAGES = 12;
const MAX_MARKDOWN_IMAGE_BYTES = 8 * 1024 * 1024;
const MARKDOWN_EXTENSIONS = new Set(["md", "markdown", "mdown", "mkd"]);

/** What the page on screen is made of: the sanitized document, the references it resolved, and the
 *  bytes behind the ones that were read. Kept beside the rendered string because that is the only
 *  place that knows what the page is actually drawing. */
interface RenderedPreview {
  checkoutId: string;
  markdownPath: string;
  html: string;
  references: readonly MarkdownImageReference[];
  /** The references a preview will read, which is the first `MAX_MARKDOWN_IMAGES` of them. The ones
   *  past that are on the page as placeholders and stay there whatever the activity names. */
  used: readonly MarkdownImageReference[];
  images: Map<string, LoadedImage>;
  /** What the loaded images cost together, so a re-read can be held to the same budget. */
  bytes: number;
  /** Whether the document names more images than a preview will read. A fact about the document,
   *  not about the read, so it survives a resource failing and coming back. */
  tooMany: boolean;
}

/** One image as the page carries it: what the command read, kept whole so a re-read can be held to
 *  the same byte budget as the first one. */
type LoadedImage = CheckoutImage;

export function isMarkdownPath(path: string): boolean {
  const extension = path.split(".").pop()?.toLowerCase();
  return extension !== undefined && MARKDOWN_EXTENSIONS.has(extension);
}

export function useMarkdownPreview(getCheckoutId: () => string | null, getOrigin: () => DocumentOrigin) {
  const markdownHtml = ref("");
  const markdownPreviewState = ref<"idle" | "loading" | "ready">("idle");
  const markdownImageWarning = ref(false);
  let generation = 0;
  let disposed = false;
  /** The page the last settled `load` published, or `null` when there is none to re-read against. */
  let rendered: RenderedPreview | null = null;
  /** How many `load` calls are still reading. Resources are only re-read when nothing else is:
   *  a render in flight owns the page and will resolve its own images anyway. */
  let loading = 0;
  /** The paths a batch of file activity moved, held until a moment no render owns the page.
   *
   *  `null` is nothing waiting. An empty array is a batch that could not say what it moved, which
   *  is not "nothing moved": every image in use is asked again when the page is re-read. */
  let queued: readonly string[] | null = null;
  /** The one re-read in flight, so two batches never copy the same map over each other.
   *
   *  It is also how the second caller waits: the pass drains whatever arrived while it was reading,
   *  and joining it costs one answer instead of a second pass over a page that is being written. */
  let pass: Promise<boolean> | null = null;

  function isCurrent(requestGeneration: number, checkoutId: string): boolean {
    return !disposed && requestGeneration === generation && getCheckoutId() === checkoutId;
  }

  function invalidate() {
    generation += 1;
  }

  function clear() {
    invalidate();
    rendered = null;
    // Nothing is left to re-read against, so what was held for the page that just went is dropped
    // with it: the next render resolves its own figures from the file it is given.
    queued = null;
    if (disposed) return;
    markdownHtml.value = "";
    markdownPreviewState.value = "idle";
    markdownImageWarning.value = false;
  }

  /** Holds the paths one batch moved for the next re-read, merged into whatever is already held.
   *
   *  Public because a caller can only know what a batch moved before it knows whether the read that
   *  batch triggered will land: a read a newer one supersedes stops at its own guard and never gets
   *  to name what brought it here, so the paths have to be with the figures already. Nothing spends
   *  them but `refreshImages` or a settled `load`. */
  function hold(touched: readonly string[]): void {
    if (queued === null) {
      queued = [...touched];
      return;
    }
    // A batch that could not say what it moved invalidates everything in use, and cannot be undone
    // by naming fewer paths than another batch did.
    if (queued.length === 0 || touched.length === 0) {
      queued = [];
      return;
    }
    const merged = new Set(queued);
    for (const path of touched) merged.add(path);
    queued = [...merged];
  }

  /**
   * Renders `source` as the preview of `path`, or leaves the page that is already up when
   * `keepVisible` says the file on screen is the one being re-read. The document is re-read twice
   * for every change anywhere in the checkout, and emptying the rendered page each time is a blink
   * the reader did nothing to cause.
   */
  async function load(checkoutId: string, path: string, source: string, keepVisible = false) {
    if (disposed) return;
    const requestGeneration = ++generation;
    loading += 1;
    try {
      await loadPage(checkoutId, path, source, keepVisible, requestGeneration);
    } finally {
      loading -= 1;
    }
    // A batch of activity that arrived while this render was still resolving named figures the page
    // has just read for itself, so it was held rather than dropped: what a read resolves is the
    // file as it was when it asked, and a write that landed during it is what this is. A newer
    // render still in flight resolves the same figures on its own, so only the last to settle comes
    // back for what was held.
    if (disposed || loading > 0 || queued === null) return;
    await refreshHeld();
  }

  async function loadPage(
    checkoutId: string,
    path: string,
    source: string,
    keepVisible: boolean,
    requestGeneration: number,
  ) {
    const settled = keepVisible && markdownPreviewState.value === "ready";
    if (!settled) {
      markdownHtml.value = "";
      markdownPreviewState.value = "loading";
    }
    markdownImageWarning.value = false;
    rendered = null;
    if (!isMarkdownPath(path)) {
      markdownPreviewState.value = "idle";
      return;
    }
    const { attachMarkdownImages, renderMarkdownPreview } = await import("../lib/markdown-preview");
    if (!isCurrent(requestGeneration, checkoutId)) return;
    const preview = await renderMarkdownPreview(source, path);
    const paths = preview.images.map((image) => image.path);
    const loadedImages = new Map<string, LoadedImage>();
    if (getOrigin() !== "checkout") {
      markdownHtml.value = attachMarkdownImages(preview.html, paths, loadedImages);
      markdownPreviewState.value = "ready";
      return;
    }
    const tooMany = preview.images.length > MAX_MARKDOWN_IMAGES;
    markdownImageWarning.value = tooMany;

    const used = preview.images.slice(0, MAX_MARKDOWN_IMAGES);
    let totalImageBytes = 0;
    for (const image of used) {
      if (!isCurrent(requestGeneration, checkoutId)) return;
      try {
        const resource = await readCheckoutMarkdownImage(checkoutId, path, image.source);
        if (!isCurrent(requestGeneration, checkoutId)) return;
        if (resource.sizeBytes > MAX_MARKDOWN_IMAGE_BYTES - totalImageBytes) {
          markdownImageWarning.value = true;
          continue;
        }
        totalImageBytes += resource.sizeBytes;
        loadedImages.set(image.path, resource);
      } catch {
        if (!isCurrent(requestGeneration, checkoutId)) return;
        markdownImageWarning.value = true;
      }
    }

    if (!isCurrent(requestGeneration, checkoutId)) return;
    rendered = {
      checkoutId,
      markdownPath: path,
      html: preview.html,
      references: preview.images,
      used,
      images: loadedImages,
      bytes: totalImageBytes,
      tooMany,
    };
    markdownHtml.value = attachMarkdownImages(preview.html, paths, loadedImages);
    markdownPreviewState.value = "ready";
  }

  /**
   * Re-reads the images the page is actually drawing, and repaints only when one of them is a path
   * a batch of file activity moved.
   *
   * A document's identity is its bytes, and the figures beside those bytes are not part of it: a
   * picture replaced by another process leaves the text exactly as it was, and a reader left looking
   * at the old one is reading a page the document no longer describes. What costs everything is
   * rendering the whole preview again, and what costs the most is what nothing on the page uses, so
   * the references the last render resolved are the ones asked again -- and only when the activity
   * named one of them. An unrelated write elsewhere in the checkout repaints nothing and reads
   * nothing.
   *
   * An empty `touched` is a batch that could not say what it moved (a burst past the watcher's own
   * path budget), which is not "nothing moved": every image in use is asked then, bounded by the
   * same count and byte budget as the first read.
   *
   * A batch that arrives while a render or another re-read owns the page is held instead of
   * dropped, and settled by whichever of those finishes last; two batches arriving together are
   * read as one pass rather than as two copies of the same map written over each other. The text is
   * never rendered again to do it: the last render's markup is what the figures are laid back onto.
   *
   * Returns whether the page was repainted, so a caller restoring a reading position only pays for
   * one that moved.
   */
  async function refreshImages(touched: readonly string[]): Promise<boolean> {
    hold(touched);
    if (disposed) return false;
    // The pass in flight drains what is held, and joining it is how this batch's caller learns
    // whether the page it was waiting on moved.
    if (pass !== null) return pass;
    // A render owns the page and resolves its own figures; `load` comes back for what was held
    // when it settles, so nothing here is lost by not asking now.
    if (loading > 0) return false;
    return refreshHeld();
  }

  /** Runs the re-reads that are held, one at a time, for as long as batches keep arriving. */
  async function refreshHeld(): Promise<boolean> {
    if (pass !== null) return pass;
    pass = drain();
    return pass;
  }

  async function drain(): Promise<boolean> {
    let repainted = false;
    try {
      // Whatever arrives while a pass is reading is read by the next turn of this loop rather than
      // by a pass of its own, which is what keeps one page from being written by two.
      while (queued !== null && !disposed) {
        const batch = queued;
        queued = null;
        if (await refreshPage(batch)) repainted = true;
      }
    } finally {
      // Cleared before the answer is handed on, so a batch queued in the same turn the loop gave up
      // starts a pass of its own instead of joining one that has already finished with the page.
      pass = null;
    }
    return repainted;
  }

  /**
   * One re-read of the references in use, against the page the last settled render published.
   *
   * Everything about the page is checked again here rather than trusted: it is the generation that
   * says a newer render replaced it, the checkout that says this is still the one on screen, and
   * the origin that says a document outside the checkout has no figures a batch can move. Only the
   * references the last render used are asked -- at most `MAX_MARKDOWN_IMAGES` of them, under the
   * same byte budget as the first read -- and the markup it produced is reused rather than rendered
   * again.
   */
  async function refreshPage(touched: readonly string[]): Promise<boolean> {
    const current = rendered;
    if (disposed || !current) return false;
    if (current.checkoutId !== getCheckoutId()) return false;
    // A review document is outside the checkout, so no batch inside it moves one of its images.
    if (getOrigin() !== "checkout") return false;
    const { attachMarkdownImages } = await import("../lib/markdown-preview");
    const requestGeneration = generation;
    const images = new Map(current.images);
    let bytes = current.bytes;
    let warning = false;

    for (const reference of current.used) {
      if (touched.length > 0 && !touched.includes(reference.path)) continue;
      // A figure that failed to load is not skipped: its path coming back in an activity is how
      // the page learns the file exists again.
      bytes -= images.get(reference.path)?.sizeBytes ?? 0;
      let next: LoadedImage | null = null;
      try {
        const resource = await readCheckoutMarkdownImage(current.checkoutId, current.markdownPath, reference.source);
        if (!isCurrent(requestGeneration, current.checkoutId)) return false;
        if (resource.sizeBytes <= MAX_MARKDOWN_IMAGE_BYTES - bytes) {
          next = resource;
          bytes += resource.sizeBytes;
        } else {
          warning = true;
        }
      } catch {
        if (!isCurrent(requestGeneration, current.checkoutId)) return false;
        warning = true;
      }
      if (next === null) images.delete(reference.path);
      else images.set(reference.path, next);
    }

    if (!isCurrent(requestGeneration, current.checkoutId)) return false;
    rendered = { ...current, images, bytes };
    markdownImageWarning.value = current.tooMany || warning;
    const html = attachMarkdownImages(
      current.html,
      current.references.map((reference) => reference.path),
      images,
    );
    if (html === markdownHtml.value) return false;
    markdownHtml.value = html;
    return true;
  }

  onBeforeUnmount(() => {
    disposed = true;
    queued = null;
    invalidate();
  });

  return {
    markdownHtml,
    markdownPreviewState,
    markdownImageWarning,
    isMarkdownPath,
    load,
    hold,
    refreshImages,
    clear,
    invalidate,
  };
}
