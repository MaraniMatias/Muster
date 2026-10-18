import { describe, expect, it, vi } from "vitest";
import { csiUChord, watchKeyboardProtocol } from "./terminal-keys";

/** The escape every one of these sequences opens with, as the code it is. */
const ESC = String.fromCharCode(0x1b);

function output(text: string): ArrayBuffer {
  const encoded = new TextEncoder().encode(text);
  return encoded.buffer.slice(encoded.byteOffset, encoded.byteOffset + encoded.byteLength) as ArrayBuffer;
}

describe("watchKeyboardProtocol", () => {
  it("leaves a program that never announced anything to be sent the line ending", () => {
    const answer = vi.fn();
    const protocol = watchKeyboardProtocol(answer);
    protocol.read(output(`${ESC}[?2026h${ESC}[38;5;196mhello`));
    expect(protocol.csiU).toBe(false);
    expect(answer).not.toHaveBeenCalled();
  });

  it("takes the encoding from the flags a program pushed, which is how a program asks for it", () => {
    const protocol = watchKeyboardProtocol(vi.fn());
    // What the protocol tells a program to emit at startup: one bit, the disambiguation.
    protocol.read(output(`hello${ESC}[>1u`));
    expect(protocol.csiU).toBe(true);
    // Still on with the flags that report event types and alternate keys alongside.
    protocol.read(output(`${ESC}[>7u`));
    expect(protocol.csiU).toBe(true);
    // Pushing nothing is a program asking for the legacy encodings back.
    protocol.read(output(`${ESC}[>0u`));
    expect(protocol.csiU).toBe(false);
  });

  it("restores the flags a program left behind when it pops", () => {
    const protocol = watchKeyboardProtocol(vi.fn());
    protocol.read(output(`${ESC}[>1u`));
    expect(protocol.csiU).toBe(true);
    // A nested push inside the alternate screen, and a pop out of it.
    protocol.read(output(`${ESC}[>0u`));
    expect(protocol.csiU).toBe(false);
    protocol.read(output(`${ESC}[<u`));
    expect(protocol.csiU).toBe(true);
    protocol.read(output(`${ESC}[<u`));
    expect(protocol.csiU).toBe(false);
  });

  it("applies the older form by the mode it is given", () => {
    const protocol = watchKeyboardProtocol(vi.fn());
    protocol.read(output(`${ESC}[=1u`));
    expect(protocol.csiU).toBe(true);
    // Mode 2 only adds, so the flag in force stays.
    protocol.read(output(`${ESC}[=2;2u`));
    expect(protocol.csiU).toBe(true);
    // Mode 3 takes away, and mode 1 replaces what was there.
    protocol.read(output(`${ESC}[=1;3u`));
    expect(protocol.csiU).toBe(false);
    protocol.read(output(`${ESC}[=1;2u`));
    expect(protocol.csiU).toBe(true);
  });

  it("answers a question with the flags in force without taking that as the program agreeing", () => {
    const answer = vi.fn();
    const protocol = watchKeyboardProtocol(answer);
    protocol.read(output(`${ESC}[?u`));
    expect(answer).toHaveBeenCalledWith(`${ESC}[?0u`);
    expect(protocol.csiU).toBe(false);
    protocol.read(output(`${ESC}[>1u`));
    protocol.read(output(`${ESC}[?u`));
    expect(answer).toHaveBeenLastCalledWith(`${ESC}[?1u`);
    expect(protocol.csiU).toBe(true);
  });

  it("reads an announcement the channel boundary cut in half", () => {
    const answer = vi.fn();
    const protocol = watchKeyboardProtocol(answer);
    protocol.read(output(`${ESC}[>`));
    expect(protocol.csiU).toBe(false);
    protocol.read(output("1u"));
    expect(protocol.csiU).toBe(true);
    // A question cut in half is one answer, not one per chunk that follows it.
    protocol.read(output(`${ESC}[?`));
    protocol.read(output("u"));
    protocol.read(output("more output"));
    expect(answer).toHaveBeenCalledTimes(1);
  });

  it("reads an announcement once however many chunks follow it", () => {
    const answer = vi.fn();
    const protocol = watchKeyboardProtocol(answer);
    protocol.read(output(`${ESC}[>1u`));
    protocol.read(output("[?25h"));
    protocol.read(output("[?25l"));
    expect(protocol.csiU).toBe(true);
    expect(answer).not.toHaveBeenCalled();
  });

  it("leaves its own answer, echoed back, as nothing to obey", () => {
    const answer = vi.fn();
    const protocol = watchKeyboardProtocol(answer);
    // A shell with echo on hands the answer straight back, and a question with flags behind it is
    // an answer rather than a question.
    protocol.read(output(`${ESC}[?0u`));
    expect(protocol.csiU).toBe(false);
    expect(answer).not.toHaveBeenCalled();
  });
});

describe("csiUChord", () => {
  const chord = (over: Partial<Parameters<typeof csiUChord>[0]>) =>
    csiUChord({ key: "", code: "", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...over });

  it("sends the chord a reader asked to be told about", () => {
    // The key is the one its position spells with nothing held down, and the shift rides in the
    // modifier byte: 1 is the base, 1 the shift and 4 the control.
    expect(chord({ key: "A", code: "KeyA", ctrlKey: true, shiftKey: true })).toBe(`${ESC}[97;6u`);
    expect(chord({ key: "P", code: "KeyP", ctrlKey: true, shiftKey: true })).toBe(`${ESC}[112;6u`);
    // A digit is the same chord as a letter, and the character the shift produced is not the key.
    expect(chord({ key: "!", code: "Digit1", ctrlKey: true, shiftKey: true })).toBe(`${ESC}[49;6u`);
  });

  it("carries the other modifiers it is handed", () => {
    expect(chord({ key: "A", code: "KeyA", ctrlKey: true, shiftKey: true, altKey: true })).toBe(`${ESC}[97;8u`);
    expect(chord({ key: "A", code: "KeyA", ctrlKey: true, shiftKey: true, metaKey: true })).toBe(`${ESC}[97;14u`);
  });

  it("leaves every chord xterm.js already spells to xterm.js", () => {
    // Without the control, the shift is a character and xterm sends it as one; without the shift,
    // the chord is a control character xterm knows how to make.
    expect(chord({ key: "A", code: "KeyA", shiftKey: true })).toBeUndefined();
    expect(chord({ key: "a", code: "KeyA", ctrlKey: true })).toBeUndefined();
    // xterm's mapping has an escape for the keys it names and sends those itself.
    expect(chord({ key: "F5", code: "F5", ctrlKey: true, shiftKey: true })).toBeUndefined();
    expect(chord({ key: "Enter", code: "Enter", ctrlKey: true, shiftKey: true })).toBeUndefined();
    // A position that is neither a letter nor a digit is not a key this build can spell.
    expect(chord({ key: "?", code: "Slash", ctrlKey: true, shiftKey: true })).toBeUndefined();
  });
});
