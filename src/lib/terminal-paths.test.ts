import { describe, expect, it } from "vitest";
import { terminalPathIn, terminalPathsIn, terminalUrlsIn } from "./terminal-paths";

/** The characters a path covers, which is what an underline has to land on. */
function at(line: string, path: string) {
  const start = line.indexOf(path);
  return { start, end: start + path.length };
}

describe("terminalPathIn", () => {
  it("takes a path out of a token and drops what framed it", () => {
    expect(terminalPathIn("src/lib/muster-terminal.ts:")).toBe("src/lib/muster-terminal.ts");
    expect(terminalPathIn("(src/App.vue)")).toBe("src/App.vue");
    expect(terminalPathIn("`Cargo.toml`")).toBe("Cargo.toml");
    expect(terminalPathIn('"my notes.md"')).toBe("my notes.md");
  });

  it("leaves off the line and column a compiler appended", () => {
    expect(terminalPathIn("src/main.rs:42:10")).toBe("src/main.rs");
    expect(terminalPathIn("src/main.rs:42")).toBe("src/main.rs");
    // A colon that is not a position is part of the name and stays.
    expect(terminalPathIn("weird:name/file.ts")).toBe("weird:name/file.ts");
  });

  it("refuses what cannot name a file", () => {
    expect(terminalPathIn("...")).toBeNull();
    expect(terminalPathIn("..")).toBeNull();
    // A flag is an argument rather than a name, and `ls -la` is full of them.
    expect(terminalPathIn("--verbose")).toBeNull();
    expect(terminalPathIn("-Wl,-x")).toBeNull();
    // A URL is a link, not a file: nothing on this disk is named that.
    expect(terminalPathIn("https://example.com/a.ts")).toBeNull();
  });

  it("offers a bare name on a line that is a listing", () => {
    // Half of what `ls` prints has no separator and no extension in it, and refusing those is
    // refusing the listing. Whether the name is a file is the probe's answer, not this one's.
    expect(terminalPathIn("Makefile")).toBe("Makefile");
    expect(terminalPathIn("LICENSE")).toBe("LICENSE");
    // A word of prose is a name on a line of its own too, and the probe is what drops it: one
    // lookup that finds nothing, rather than a guess that could never change.
    expect(terminalPathIn("error")).toBe("error");
    // A sentence is not a listing, so its words are not names.
    expect(terminalPathIn("error: something broke")).toBeNull();
  });
});

describe("terminalPathsIn", () => {
  it("reads every path off a line, in the order printed", () => {
    const line = "error[E0308]: mismatched types → src/lib/foo.rs:12:5 and src/main.rs";
    expect(terminalPathsIn(line)).toEqual([
      { path: "src/lib/foo.rs", ...at(line, "src/lib/foo.rs") },
      { path: "src/main.rs", ...at(line, "src/main.rs") },
    ]);
  });

  it("does not find a quoted path twice", () => {
    const line = 'could not read "src/lib/foo.ts"';
    expect(terminalPathsIn(line)).toEqual([{ path: "src/lib/foo.ts", ...at(line, "src/lib/foo.ts") }]);
  });

  it("keeps the underline off the quote and the line number", () => {
    const line = '  in "src/App.vue:12:4", and again in src/App.vue';
    const [first, second] = terminalPathsIn(line);
    // The quoted one stops at the file, not at the `:12:4` that framed it.
    expect(first).toEqual({ path: "src/App.vue", start: 6, end: 17 });
    expect(second).toEqual({ path: "src/App.vue", start: 38, end: 49 });
  });

  it("keeps the ./ a shell prints, because that is the file it means", () => {
    expect(terminalPathsIn("writing ./src/App.vue")).toEqual([{ path: "./src/App.vue", start: 8, end: 21 }]);
  });

  it("reads only the name field from basic long `ls` rows", () => {
    const file = "-rw-r--r--  1 user  staff  123 Oct 11 12:34 LICENSE";
    expect(terminalPathsIn(file)).toEqual([{ path: "LICENSE", ...at(file, "LICENSE") }]);

    // A long-listing name may contain spaces; the whole basename and its original columns survive.
    const spaced = "-rw-r--r--  1 user  staff  123 Oct 11 2026 release notes";
    expect(terminalPathsIn(spaced)).toEqual([{ path: "release notes", ...at(spaced, "release notes") }]);

    const directory = "drwxr-xr-x  5 user  staff  160 Oct 11 12:34 src";
    expect(terminalPathsIn(directory)).toEqual([{ path: "src", ...at(directory, "src") }]);

    const symlink = "lrwxr-xr-x  1 user  staff  9 Oct 11 12:34 alias with spaces -> docs/dest file";
    expect(terminalPathsIn(symlink)).toEqual([{ path: "alias with spaces", ...at(symlink, "alias with spaces") }]);

    // The arrow is only a target separator for symlink rows; a regular basename may contain it.
    const regular = "-rw-r--r--  1 user  staff  123 Oct 11 12:34 file -> archive";
    expect(terminalPathsIn(regular)).toEqual([{ path: "file -> archive", ...at(regular, "file -> archive") }]);

    // A leading-dash name is still rejected by the path candidate guard.
    expect(terminalPathsIn("-rw-r--r--  1 user staff  123 Oct 11 12:34 --verbose")).toEqual([]);
  });

  it("offers the bare names on a listing and not the words on a sentence", () => {
    // What `ls` prints in columns: the extension-less names are the ones that used to be
    // unopenable, and the probe says which of them are files.
    const columns = "AGENTS.md   CHANGELOG.md   LICENSE   TODO.md   src";
    expect(terminalPathsIn(columns).map((candidate) => candidate.path)).toEqual([
      "AGENTS.md",
      "CHANGELOG.md",
      "LICENSE",
      "TODO.md",
      "src",
    ]);

    // What `ls -1` prints: one name on a line of its own, which is a listing too.
    expect(terminalPathsIn("Makefile").map((candidate) => candidate.path)).toEqual(["Makefile"]);

    // A sentence is not a listing, so none of its words cost a lookup.
    expect(terminalPathsIn("error: something broke")).toEqual([]);
    expect(terminalPathsIn("")).toEqual([]);
    expect(terminalPathsIn("....")).toEqual([]);
  });
});

describe("terminalUrlsIn", () => {
  it("reads the address off the line curl printed", () => {
    const line = "curl https://www.example.com/ -o /dev/null";
    expect(terminalUrlsIn(line)).toEqual([
      { url: "https://www.example.com/", ...at(line, "https://www.example.com/") },
    ]);
  });

  it("reads every address off a line, in the order printed", () => {
    const line = "docs at https://example.com/guide, mirror http://127.0.0.1:1420/";
    expect(terminalUrlsIn(line)).toEqual([
      { url: "https://example.com/guide", ...at(line, "https://example.com/guide") },
      { url: "http://127.0.0.1:1420/", ...at(line, "http://127.0.0.1:1420/") },
    ]);
  });

  it("keeps a parenthesis the address itself carries", () => {
    // The trailing `)` is the sentence's and goes; the one inside the title is part of the page
    // and stays, because trimming it hands the browser an article that does not exist.
    const line = "see https://en.wikipedia.org/wiki/Terminal_emulator_(OS) for more";
    const url = "https://en.wikipedia.org/wiki/Terminal_emulator_(OS)";
    expect(terminalUrlsIn(line)).toEqual([{ url, ...at(line, url) }]);
  });

  it("keeps the underline off a quoted address", () => {
    const line = 'npm WARN deprecated, see "https://example.com/migration" instead';
    expect(terminalUrlsIn(line)).toEqual([
      { url: "https://example.com/migration", ...at(line, "https://example.com/migration") },
    ]);
  });

  it("refuses everything that is not a page", () => {
    // The opener on the other end opens `http` and `https` and nothing else, so a scheme that names
    // a file, an app or the platform gets no underline rather than a link that opens the wrong thing.
    for (const line of [
      "file:///etc/passwd",
      "vscode://file/tmp/x",
      "mailto:someone@example.com",
      "javascript:alert(1)",
      // No scheme at all, which is a word in a sentence rather than an address.
      "example.com",
      "see example.com/docs for more",
      // A quoted address with a space in it is one the opener would refuse as a command line.
      '"https://example.com/a b"',
      "",
    ]) {
      expect(terminalUrlsIn(line)).toEqual([]);
    }
  });

  it("keeps a page out of the paths, because a page is not a file here", () => {
    expect(terminalPathsIn("curl https://www.example.com/ now")).toEqual([]);
  });
});
