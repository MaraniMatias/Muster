import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { suppressXtermDebugLogs, xtermDebugLogPlugin } from "./xterm-debug-log-plugin.mjs";

const ROOT = resolve(import.meta.dirname, "..");
const MODULES = [
  ["@xterm/xterm/lib/xterm.mjs", "/repo/node_modules/@xterm/xterm/lib/xterm.mjs"],
  ["@xterm/addon-webgl/lib/addon-webgl.mjs", "/repo/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs"],
  [
    "@xterm/addon-unicode11/lib/addon-unicode11.mjs",
    "/repo/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs",
  ],
];
const LISTENER_ERROR = 'new Error("Attempted to dispose unknown listener")';

function emitterFragment(modulePath) {
  const source = readFileSync(resolve(ROOT, "node_modules", modulePath), "utf8");
  const start = source.indexOf('throw console.log("disposed?"');
  const marker = source.indexOf(LISTENER_ERROR, start);
  assert.notEqual(start, -1, `${modulePath} must contain the emitter throw`);
  assert.notEqual(marker, -1, `${modulePath} must retain the unknown-listener error`);
  return source.slice(start, marker + LISTENER_ERROR.length) + ";";
}

test("collapses the three debug logs into one console.error in the three installed xterm modules", () => {
  const plugin = xtermDebugLogPlugin();
  assert.equal(plugin.apply, "build", "the transform must stay build-only so dev keeps the original logs");
  for (const [modulePath, id] of MODULES) {
    const result = plugin.transform(emitterFragment(modulePath), id);
    assert.ok(result, `${modulePath} emitter must be transformed`);
    assert.doesNotMatch(result.code, /console\.log\("(?:disposed|size|arr)\?"/);
    assert.match(
      result.code,
      /console\.error\("disposed\?", this\._disposed, "size\?", this\._size, "arr\?", JSON\.stringify\(this\._listeners\)\)/,
      `${modulePath} must keep the emitted debug context on the error path`,
    );
    assert.ok(result.code.includes(LISTENER_ERROR));
  }
});

test("keeps failure-path work, the exact throw and the debug context when the error path runs", () => {
  const source = `
    const data = { toJSON() { events.push("toJSON"); if (throwFromJSON) throw new Error("toJSON failed"); return "encoded"; } };
    function mark(value) { events.push(value); return value; }
    try {
      throw console.log("disposed?", mark("first")), console.log("size?", mark("second")), console.log("arr?", JSON.stringify(data)), new Error("Attempted to dispose unknown listener");
    } catch (error) { events.push(["caught", error.message]); }
  `;
  const transformed = suppressXtermDebugLogs(source);
  assert.ok(transformed);
  assert.ok(transformed.includes("JSON.stringify(data)"), "failure-path serialization remains evaluated");
  assert.match(
    transformed,
    /console\.error\("disposed\?", mark\("first"\), "size\?", mark\("second"\), "arr\?", JSON\.stringify\(data\)\)/,
    "the labels and evaluated values are kept in a single call",
  );
  assert.ok(
    transformed.includes('new Error("Attempted to dispose unknown listener")'),
    "the thrown error must be untouched",
  );

  const execute = (code, throwFromJSON) => {
    const events = [];
    runInNewContext(code, {
      events,
      throwFromJSON,
      console: {
        log(...args) {
          events.push(["console.log", args]);
        },
        error(...args) {
          events.push(["console.error", args]);
        },
      },
    });
    return JSON.parse(JSON.stringify(events));
  };

  assert.deepEqual(execute(transformed, false), [
    "first",
    "second",
    "toJSON",
    ["console.error", ["disposed?", "first", "size?", "second", "arr?", '"encoded"']],
    ["caught", "Attempted to dispose unknown listener"],
  ]);

  // A JSON.stringify failure on the error path must still escape before the Error is thrown.
  assert.deepEqual(
    execute(transformed, true),
    execute(source, true).filter((event) => event[0] !== "console.log"),
  );
});

test("only a complete exact unknown-listener throw is eligible; other logs remain", () => {
  const plugin = xtermDebugLogPlugin();
  const id = MODULES[0][1];
  const exact = emitterFragment(MODULES[0][0]);
  const similarThrow =
    'throw console.log("disposed?",x),console.log("size?",y),console.log("arr?",z),new Error("Attempted to dispose unknown listener");';
  const nonEmitterLogs = 'console.log("disposed?",x);console.log("size?",y);console.log("arr?",z);';

  assert.equal(plugin.transform(exact, "/repo/src/main.ts"), null);
  assert.equal(
    plugin.transform(similarThrow, "/repo/node_modules/@xterm/addon-serialize/lib/addon-serialize.mjs"),
    null,
  );
  assert.equal(plugin.transform(similarThrow, "/repo/node_modules/@xterm/xterm/lib/other.mjs"), null);
  assert.equal(plugin.transform(similarThrow, "/repo/node_modules/@xterm/xterm/lib/xterm.js"), null);
  assert.equal(plugin.transform(nonEmitterLogs, id), null);
  assert.equal(plugin.transform(similarThrow.replace('"disposed?"', '"disposed? extra"'), id), null);
  assert.equal(
    plugin.transform(similarThrow.replace('"Attempted to dispose unknown listener"', '"other error"'), id),
    null,
  );

  const withOtherLogs = `console.log("disposed? extra", keep()); console.warn("keep"); console.error("keep"); ${exact}`;
  const result = plugin.transform(withOtherLogs, id);
  assert.ok(result);
  assert.match(result.code, /console\.log\("disposed\? extra", keep\(\)\)/);
  assert.match(result.code, /console\.warn\("keep"\)/);
  assert.match(result.code, /console\.error\("keep"\)/);
  assert.equal(
    result.code.match(/console\.error\(/g).length,
    2,
    "only the pre-existing error and the emitter branch log; nothing is added outside it",
  );
});
