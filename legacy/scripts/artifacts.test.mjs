import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DIST = resolve(ROOT, "dist");
const sha256 = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");
const EXPECTED_FONT_SHA256 = {
  "FiraCode-Bold.woff2": "579875c4041100ef9ba140b0c38a52979438d987d1708771018a69c9bdfa21a9",
  "FiraCode-Regular.woff2": "dd1de59cd3efe47a3aad6e9cbc93a8fe25685085074f9fed383ac5107c20145f",
  "NerdSymbols.woff2": "884f36993b24d91233b7da36a4994ce6d2dc23520469a13f60a0d537aee843a2",
};

test("production dist preserves third-party notices and shipped font binaries", () => {
  const catppuccin = readFileSync(resolve(DIST, "assets/Catppuccin-MIT.txt"), "utf8");
  const iconSource = readFileSync(resolve(ROOT, "src/lib/catppuccin-icons.ts"), "utf8");
  const sourceNotice = iconSource.match(/\/\/     MIT License\n(?:\/\/(?:     .*)?\n)+/)?.[0];
  assert.ok(sourceNotice, "Catppuccin source comment must retain its complete MIT notice");
  assert.equal(catppuccin, `${sourceNotice.replace(/^\/\/(?:     )?/gm, "").trimEnd()}\n`);
  assert.ok(
    readdirSync(resolve(DIST, "assets"))
      .filter((name) => name.endsWith(".js"))
      .some((name) => readFileSync(resolve(DIST, "assets", name), "utf8").includes("folder_admin")),
    "the generated Catppuccin icon data must still be shipped",
  );

  const firaLicense = readFileSync(resolve(DIST, "assets/FiraCode-OFL-1.1.txt"), "utf8");
  assert.equal(firaLicense, readFileSync(resolve(ROOT, "src/assets/fonts/LICENSE-FiraCode.txt"), "utf8"));
  assert.match(firaLicense, /Copyright \(c\) 2014, The Fira Code Project Authors/);
  assert.match(firaLicense, /SIL OPEN FONT LICENSE Version 1\.1/);
  assert.match(firaLicense, /TERMINATION[\s\S]*DISCLAIMER[\s\S]*OTHER DEALINGS IN THE FONT SOFTWARE\./);
  assert.equal(
    readFileSync(resolve(DIST, "licenses/Muster-MIT.txt"), "utf8"),
    readFileSync(resolve(ROOT, "LICENSE"), "utf8"),
  );

  const fontDirectory = resolve(ROOT, "src/assets/fonts");
  const fontNames = Object.keys(EXPECTED_FONT_SHA256).sort();
  assert.deepEqual(
    readdirSync(fontDirectory)
      .filter((name) => name.endsWith(".woff2"))
      .sort(),
    fontNames,
  );
  const sourceFonts = fontNames
    .map((name) => {
      const digest = sha256(resolve(fontDirectory, name));
      assert.equal(digest, EXPECTED_FONT_SHA256[name], name);
      return digest;
    })
    .sort();
  const bundledFonts = readdirSync(resolve(DIST, "assets"))
    .filter((name) => /\.(?:ttf|woff2)$/.test(name))
    .map((name) => sha256(resolve(DIST, "assets", name)))
    .sort();
  assert.deepEqual(bundledFonts, sourceFonts);

  const thirdParty = readFileSync(resolve(DIST, "licenses/THIRD-PARTY-NOTICES.txt"), "utf8");
  assert.match(thirdParty, /JavaScript production dependencies/);
  assert.match(thirdParty, /Rust production dependencies/);
  assert.match(thirdParty, /target-conditional crates are included, but not every crate appears in every artifact/);
  assert.match(thirdParty, /Linux GTK\/WebKit or other dynamically linked system libraries/);
  assert.match(thirdParty, /Packages without installed or pinned license text are listed by declared metadata/);
  assert.match(thirdParty, /FiraCode-Regular\.woff2 and FiraCode-Bold\.woff2/);
  assert.match(thirdParty, /NerdSymbols\.woff2/);
  assert.match(thirdParty, /no verified upstream glyph-set license inventory/);
  assert.match(thirdParty, /@tauri-apps\/api@/);
  assert.match(thirdParty, /serde@/);
  assert.match(thirdParty, /alloc-stdlib@0\.3\.0 — BSD-3-Clause \[pinned upstream license text\]/);
  assert.match(thirdParty, /clipboard-win@5\.4\.1 — BSL-1\.0 \[pinned upstream license text\]/);
  assert.match(thirdParty, /sidecar SHA-256: c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae/);
  assert.match(
    thirdParty,
    /github\.com\/knurling-rs\/defmt\/blob\/4a8cdb44891ed57b8ff5a023b6bec7137c48708f\/LICENSE-MIT/,
  );
  for (const file of [
    "alloc-stdlib-0.3.0-LICENSE.txt",
    "clipboard-win-5.4.1-LICENSE.txt",
    "defmt-parser-1.0.0-LICENSE-MIT.txt",
    "dlopen2-0.8.2-LICENSE.txt",
    "ndk-0.9.0-LICENSE-MIT.txt",
  ]) {
    assert.ok(
      thirdParty.includes(readFileSync(resolve(ROOT, "scripts/third-party-licenses", file), "utf8").trimEnd()),
      `${file} should be embedded in the aggregate notice`,
    );
  }
  assert.ok(thirdParty.includes(catppuccin));
  assert.ok(thirdParty.includes(firaLicense.trimEnd()));
  assert.ok(thirdParty.includes(readFileSync(resolve(ROOT, "LICENSE"), "utf8").trimEnd()));
});

test("Tauri frontendDist points at the verified production artifact", () => {
  const configPath = resolve(ROOT, "src-tauri/tauri.conf.json");
  const config = JSON.parse(readFileSync(configPath, "utf8"));
  assert.equal(resolve(dirname(configPath), config.build.frontendDist), DIST);
  const debNotice = config.bundle.linux.deb.files["/usr/share/doc/muster/THIRD-PARTY-NOTICES.txt"];
  assert.equal(resolve(dirname(configPath), debNotice), resolve(DIST, "licenses/THIRD-PARTY-NOTICES.txt"));
});

test("production JS retains xterm's listener failure", () => {
  const javascript = readdirSync(resolve(DIST, "assets"))
    .filter((name) => name.endsWith(".js"))
    .map((name) => readFileSync(resolve(DIST, "assets", name), "utf8"))
    .join("\n");

  assert.doesNotMatch(javascript, /console\.log\("(?:disposed|size|arr)\?"/);
  assert.match(javascript, /new Error\("Attempted to dispose unknown listener"\)/);
});
