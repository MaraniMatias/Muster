// @vitest-environment happy-dom
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick, ref } from "vue";
import type { Ref } from "vue";

const mocks = vi.hoisted(() => ({
  /**
   * The reads a test is holding back, keyed by the text the read was asked for. Keying on the text
   * rather than on a counter is what keeps a test independent of which other tests ran first and
   * left a read of their own in flight.
   */
  held: new Map<string, Promise<void>>(),
  /** Every grammar the reading was asked for, and the text it was asked about. */
  asked: [] as Array<{ language: string; source: string }>,
  reject: new Set<string>(),
}));

vi.mock("../lib/source-highlighter", () => ({
  highlightSourceAs: async (language: string, source: string) => {
    mocks.asked.push({ language, source });
    await mocks.held.get(source);
    if (mocks.reject.has(source)) throw new Error("no grammar");
    return source.split("\n").map((line) => `<span>${line}</span>`);
  },
}));

import { useSourceHighlight } from "./source-highlight";

function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

interface Host {
  wrapper: ReturnType<typeof mount>;
  highlighter: ReturnType<typeof useSourceHighlight>;
  text: () => string;
  content: Ref<string>;
  fileIdentity: Ref<string | null>;
  language: Ref<string | null>;
  markdownPreview: Ref<boolean>;
}

/** A host that draws only what the reading published, so a test reads what is on screen. */
function host(): Host {
  const content = ref("const a = 1;");
  const fileIdentity = ref<string | null>("checkout:a\0checkout\0src/app.ts");
  const language = ref<string | null>("typescript");
  const markdownPreview = ref(false);
  let highlighter!: ReturnType<typeof useSourceHighlight>;
  const wrapper = mount(
    defineComponent({
      setup() {
        highlighter = useSourceHighlight({
          content: () => content.value,
          language: () => language.value,
          fileIdentity: () => fileIdentity.value,
          markdownPreview: () => markdownPreview.value,
        });
        return () =>
          h(
            "div",
            [
              highlighter.highlighting.value ? "reading" : "idle",
              `|lines:${highlighter.highlightedLines.value?.length ?? "none"}`,
            ].join(""),
          );
      },
    }),
  );
  return { wrapper, highlighter, text: () => wrapper.text(), content, fileIdentity, language, markdownPreview };
}

describe("useSourceHighlight", () => {
  beforeEach(() => {
    mocks.held.clear();
    mocks.reject.clear();
    mocks.asked.length = 0;
  });

  it("reads the text on screen with the grammar the file is read as", async () => {
    const panel = host();
    panel.highlighter.startHighlight(panel.fileIdentity.value!, panel.content.value);
    await flushPromises();

    expect(mocks.asked).toEqual([{ language: "typescript", source: "const a = 1;" }]);
    expect(panel.text()).toBe("idle|lines:1");

    panel.wrapper.unmount();
  });

  it("never publishes the answer of a reading that was asked for earlier", async () => {
    const first = gate();
    const second = gate();
    mocks.held.set("const a = 1;", first.promise);
    const panel = host();

    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();
    expect(panel.text()).toBe("reading|lines:none");

    // The user types while the reading of what they typed first is still on its way.
    panel.content.value = "const b = 2;";
    mocks.held.set("const b = 2;", second.promise);
    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const b = 2;");
    await flushPromises();
    expect(mocks.asked).toHaveLength(2);

    // The stale answer lands first and is worth nothing: it is of text that is no longer on screen.
    first.release();
    await flushPromises();
    expect(panel.text()).toBe("reading|lines:none");

    second.release();
    await flushPromises();
    expect(panel.text()).toBe("idle|lines:1");

    panel.wrapper.unmount();
  });

  it("leaves the screen alone when the file on it is not the one that was read", async () => {
    const held = gate();
    mocks.held.set("const a = 1;", held.promise);
    const panel = host();

    panel.highlighter.startHighlight("checkout:a\0checkout\0src/app.ts", "const a = 1;");
    await flushPromises();

    panel.fileIdentity.value = "checkout:a\0checkout\0src/other.ts";
    held.release();
    await flushPromises();
    expect(panel.text()).toBe("reading|lines:none");

    panel.wrapper.unmount();
  });

  it("gives up on the reading when the pane starts drawing a Markdown preview", async () => {
    const held = gate();
    mocks.held.set("const a = 1;", held.promise);
    const panel = host();

    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();

    // The preview renders its own fences, so a reading of the source underneath says nothing here.
    panel.markdownPreview.value = true;
    held.release();
    await flushPromises();
    expect(panel.text()).toBe("reading|lines:none");

    panel.wrapper.unmount();
  });

  it("leaves the painted lines alone when a reading is given up on without clearing", async () => {
    const panel = host();
    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();
    expect(panel.text()).toBe("idle|lines:1");

    // A view that is about to be asked again should not blink on the way there, so what is already
    // painted stays until the next reading lands.
    panel.highlighter.invalidateHighlight(false);
    await nextTick();
    expect(panel.text()).toBe("idle|lines:1");

    panel.wrapper.unmount();
  });

  it("takes the painted lines with it when the reading is given up on for good", async () => {
    const panel = host();
    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();
    expect(panel.text()).toBe("idle|lines:1");

    // The Code view takes the Shiki lines away with it, which is the reason one invalidation clears
    // and another does not.
    panel.highlighter.invalidateHighlight();
    await nextTick();
    expect(panel.text()).toBe("idle|lines:none");

    panel.wrapper.unmount();
  });

  it("asks for nothing when no grammar is loaded for the file", async () => {
    const panel = host();
    panel.language.value = null;

    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();

    expect(mocks.asked).toEqual([]);
    expect(panel.text()).toBe("idle|lines:none");

    panel.wrapper.unmount();
  });

  it("asks for nothing when there is no document on screen", async () => {
    const panel = host();
    panel.fileIdentity.value = null;

    panel.highlighter.startHighlight("checkout:a\0checkout\0src/app.ts", "const a = 1;");
    await flushPromises();

    expect(mocks.asked).toEqual([]);

    panel.wrapper.unmount();
  });

  it("stops reading rather than leaving the pane waiting on a grammar that will not load", async () => {
    mocks.reject.add("const a = 1;");
    const panel = host();

    panel.highlighter.startHighlight(panel.fileIdentity.value!, "const a = 1;");
    await flushPromises();

    // A grammar that is not there is not a failure the reader has to hear about, but the pane
    // cannot sit on "reading" for one forever either.
    expect(panel.text()).toBe("idle|lines:none");

    panel.wrapper.unmount();
  });
});
