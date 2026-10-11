// @vitest-environment happy-dom
// One composable per file, mounted the way the pane mounts it, with the shell that draws what it
// published rather than the pane's own machinery around it.
import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, ref } from "vue";
import type { CheckoutImage } from "../domain/files";
import type { DocumentOrigin } from "../domain/main-document";
import { useMarkdownPreview } from "./markdown-preview";

const mocks = vi.hoisted(() => ({ readCheckoutMarkdownImage: vi.fn() }));

vi.mock("../lib/ipc", () => ({
  readCheckoutMarkdownImage: mocks.readCheckoutMarkdownImage,
}));

/** An image, named by the bytes it carries so a repaint is visible in the page it lands on. */
function image(marker: string): CheckoutImage {
  return { mimeType: "image/png", dataBase64: btoa(marker), sizeBytes: 8 };
}

/** The `src` of every figure the page is drawing, in document order. */
function sourcesOf(html: string): string[] {
  return [...html.matchAll(/<img src="data:image\/png;base64,([^"]+)"/g)].map((match) => atob(match[1]));
}

/** A promise a test releases by hand, for a read that must finish on its own schedule. */
function deferred<T>() {
  let release!: (value: T) => void;
  let fail!: (error: unknown) => void;
  const promise = new Promise<T>((resolve, reject) => {
    release = resolve;
    fail = reject;
  });
  return { promise, release, fail };
}

interface Host {
  wrapper: ReturnType<typeof mount>;
  preview: ReturnType<typeof useMarkdownPreview>;
  checkout: ReturnType<typeof ref<string | null>>;
  origin: ReturnType<typeof ref<DocumentOrigin>>;
  /** What the page is drawing, which is what a reader would be looking at. */
  sources: () => string[];
  /** The path each read was asked for, so a test can name what was read rather than how often. */
  reads: () => string[];
}

function host(): Host {
  const checkout = ref<string | null>("checkout:one");
  const origin = ref<DocumentOrigin>("checkout");
  let preview!: ReturnType<typeof useMarkdownPreview>;
  const wrapper = mount(
    defineComponent({
      setup() {
        preview = useMarkdownPreview(
          () => checkout.value,
          () => origin.value,
        );
        return () => h("div", preview.markdownHtml.value);
      },
    }),
  );
  const reads = () => mocks.readCheckoutMarkdownImage.mock.calls.map((call) => call[2] as string);
  return { wrapper, preview, checkout, origin, sources: () => sourcesOf(preview.markdownHtml.value), reads };
}

const DOC = "![one](images/one.png)\n\n![two](images/two.png)";

beforeEach(() => {
  vi.clearAllMocks();
});

describe("useMarkdownPreview", () => {
  /**
   * The write that landed while the render was reading is the whole hazard: the assets a read
   * resolves are the file as it was when it asked, so a batch of activity that arrives mid-render
   * is a fact about a page that is about to be published. It used to be dropped on the floor, and
   * the page went up holding an asset another process had already replaced.
   */
  it("settles the activity that arrived while a render was still reading", async () => {
    const page = host();
    const duringLoad = deferred<CheckoutImage>();
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(() => duringLoad.promise).mockResolvedValue(image("after"));

    const rendering = page.preview.load("checkout:one", "docs/readme.md", DOC);
    // The render is inside its figure reads, so this is a batch of activity against a page that
    // does not exist yet.
    expect(await page.preview.refreshImages(["docs/images/one.png"])).toBe(false);
    duringLoad.release(image("before"));
    await rendering;

    // Whatever the render published, the batch that arrived during it is read afterwards, so the
    // page ends up holding the file as it is now rather than as it was mid-read.
    await vi.waitFor(() => expect(page.sources()).toEqual(["after", "after"]));
    // The render read both, then only the figure the batch named: nothing else was asked again.
    expect(page.reads()).toEqual(["images/one.png", "images/two.png", "images/one.png"]);
    expect(page.preview.markdownPreviewState.value).toBe("ready");
  });

  /**
   * Two batches arriving together used to run as two passes over one page: each copied the map the
   * other was writing, so whichever finished last published a page missing the other's figure.
   */
  it("reads two overlapping batches as one pass instead of two copies of the same page", async () => {
    const page = host();
    mocks.readCheckoutMarkdownImage.mockResolvedValue(image("first"));
    await page.preview.load("checkout:one", "docs/readme.md", DOC);
    expect(page.sources()).toEqual(["first", "first"]);

    // Both batches are held inside their own reads, which is where two passes over one page used
    // to be possible at all.
    const firstRead = deferred<CheckoutImage>();
    const secondRead = deferred<CheckoutImage>();
    mocks.readCheckoutMarkdownImage
      .mockImplementationOnce(() => firstRead.promise)
      .mockImplementationOnce(() => secondRead.promise)
      .mockResolvedValue(image("after"));

    const one = page.preview.refreshImages(["docs/images/one.png"]);
    const two = page.preview.refreshImages(["docs/images/two.png"]);
    firstRead.release(image("one-new"));
    secondRead.release(image("two-new"));
    const repainted = await Promise.all([one, two]);

    // Both answers agree, because there was one page and it was written once: neither figure is
    // left holding the bytes the other pass published over it, and each is read exactly once.
    expect(repainted).toEqual([true, true]);
    expect(page.reads()).toEqual(["images/one.png", "images/two.png", "images/one.png", "images/two.png"]);
    expect(page.sources()).toEqual(["one-new", "two-new"]);
  });

  /** A batch that cannot say what it moved asks for everything in use, and cannot be undone by a
   *  later batch that names less. */
  it("keeps a batch that could not say what it moved when another names one path", async () => {
    const page = host();
    const duringLoad = deferred<CheckoutImage>();
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(() => duringLoad.promise).mockResolvedValue(image("first"));
    const rendering = page.preview.load("checkout:one", "docs/readme.md", DOC);
    // Both batches arrive while the render still owns the page, so both are held rather than read,
    // and the second is merged into what the first left waiting.
    void page.preview.refreshImages([]);
    void page.preview.refreshImages(["docs/images/one.png"]);
    duringLoad.release(image("first"));
    await rendering;

    // What was held is still everything: a batch that could not say what it moved is not narrowed by
    // one that came after it and named less.
    expect(page.sources()).toEqual(["first", "first"]);
    expect(page.reads()).toEqual(["images/one.png", "images/two.png", "images/one.png", "images/two.png"]);
  });

  /** A page nobody is looking at any more is not republished, and nothing queued for it survives. */
  it("leaves a page that was disposed mid-read alone and comes back for nothing", async () => {
    const page = host();
    mocks.readCheckoutMarkdownImage.mockResolvedValue(image("first"));
    await page.preview.load("checkout:one", "docs/readme.md", DOC);
    const before = page.preview.markdownHtml.value;

    const held = deferred<CheckoutImage>();
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(() => held.promise);
    const refreshing = page.preview.refreshImages(["docs/images/one.png"]);
    page.wrapper.unmount();
    held.release(image("after"));
    await refreshing;

    expect(page.preview.markdownHtml.value).toBe(before);
    expect(await page.preview.refreshImages(["docs/images/one.png"])).toBe(false);
    expect(page.reads()).toHaveLength(3);
  });

  /**
   * A newer render replaces the page and answers the batch that was held for the older one: the
   * figures on screen are the ones the document that is up names, read as of now.
   */
  it("drains what was held when a new render replaces the page", async () => {
    const page = host();
    mocks.readCheckoutMarkdownImage.mockResolvedValue(image("first"));
    await page.preview.load("checkout:one", "docs/readme.md", DOC);

    const held = deferred<CheckoutImage>();
    mocks.readCheckoutMarkdownImage.mockImplementationOnce(() => held.promise);
    const heldDuringLoad = page.preview.load("checkout:one", "docs/other.md", "![three](images/three.png)");
    void page.preview.refreshImages(["docs/images/three.png"]);
    held.release(image("three"));
    mocks.readCheckoutMarkdownImage.mockResolvedValue(image("after"));
    await heldDuringLoad;

    await vi.waitFor(() => expect(page.sources()).toEqual(["after"]));
    expect(page.preview.markdownPreviewState.value).toBe("ready");
  });

  /** What is on screen decides whether anything is asked, not the other way round. */
  it("asks for nothing on a page outside the checkout, and repaints nothing either", async () => {
    const page = host();
    page.origin.value = "review-workdir";
    await page.preview.load("checkout:one", "docs/readme.md", DOC);
    expect(page.reads()).toHaveLength(0);

    expect(await page.preview.refreshImages(["docs/images/one.png"])).toBe(false);
    expect(page.reads()).toHaveLength(0);
    expect(page.sources()).toEqual([]);
  });
});
