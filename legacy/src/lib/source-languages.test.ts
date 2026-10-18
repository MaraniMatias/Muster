import { describe, expect, it } from "vitest";
import { languageForName } from "./source-highlighter";
import {
  LANGUAGE_BY_EXTENSION,
  LANGUAGE_BY_FILENAME,
  PLAIN_TEXT,
  SELECTABLE_LANGUAGES,
  detectedLanguageName,
  languageExtensions,
  languageLabel,
} from "./source-languages";

describe("source languages", () => {
  it("detects the grammar a path's extension asks for, whatever the case", () => {
    expect(detectedLanguageName("src/app.TS")).toBe("typescript");
    expect(detectedLanguageName("config/app.yml")).toBe("yaml");
    expect(detectedLanguageName("src/main.rs")).toBe("rust");
    // An extension that names no grammar is a plain file, not a guess at one.
    expect(detectedLanguageName("notes.txt")).toBeUndefined();
    expect(detectedLanguageName("")).toBeUndefined();
  });

  it("detects the dotfiles whose leading dot is the whole name, and the tail of .env", () => {
    expect(detectedLanguageName(".gitignore")).toBe("gitignore");
    // The basename decides, so a nested dotfile reads like the root one.
    expect(detectedLanguageName("packages/app/.gitignore")).toBe("gitignore");
    expect(detectedLanguageName(".gitconfig")).toBe("ini");
    expect(detectedLanguageName(".gitattributes")).toBe("ini");
    expect(detectedLanguageName(".prettierrc")).toBe("json");
    // `.env` is a format, not a name: the tail is unbounded, and the dot keeps `.envrc` out.
    expect(detectedLanguageName(".env")).toBe("dotenv");
    expect(detectedLanguageName(".env.local")).toBe("dotenv");
    expect(detectedLanguageName(".env.production.local")).toBe("dotenv");
    expect(detectedLanguageName("apps/api/.env.test")).toBe("dotenv");
    expect(detectedLanguageName(".envrc")).toBeUndefined();
    // A dotfile with a real extension still resolves on the extension.
    expect(detectedLanguageName(".eslintrc.json")).toBe("json");
  });

  it("reads a shell startup file by its name, since its leading dot is its whole extension", () => {
    // Every shell spells the same file its own way, and `zshrc` is not a suffix anyone registered,
    // so these are names rather than extensions: without the table a `.zshrc` renders as plain text.
    expect(detectedLanguageName(".zshrc")).toBe("shellscript");
    expect(detectedLanguageName(".zshenv")).toBe("shellscript");
    expect(detectedLanguageName(".bashrc")).toBe("shellscript");
    expect(detectedLanguageName(".bash_profile")).toBe("shellscript");
    expect(detectedLanguageName(".profile")).toBe("shellscript");
    // A name that is only ever a suffix still resolves on it, and the two never disagree.
    expect(detectedLanguageName("deploy.zsh")).toBe("shellscript");
  });

  it("reads a dotfile wearing a variant of its own name as the file it is a variant of", () => {
    // A dotfiles checkout is a checkout like any other, so the name somebody gave the file on their
    // machine is the name it has here. The format is the part before the second dot.
    expect(detectedLanguageName(".zshrc.local")).toBe("shellscript");
    expect(detectedLanguageName(".bashrc.macos")).toBe("shellscript");
    expect(detectedLanguageName("dotfiles/.zprofile.work")).toBe("shellscript");
    // The dot after the name is what keeps this off a file that is only named after one.
    expect(detectedLanguageName(".tmux.conf")).toBe("ini");
    expect(detectedLanguageName(".dircolors.local")).toBe("ini");
    // A tail that names no dotfile of its own leaves the file plain rather than borrowing one.
    expect(detectedLanguageName(".editor.bak")).toBeUndefined();
  });

  it("names every grammar it detects, and every grammar the toolbar offers", () => {
    // The table is split across two modules, so this is what keeps a renamed grammar from turning
    // into a silent "no highlighting" for a file that used to have it.
    for (const name of new Set(LANGUAGE_BY_EXTENSION.values())) expect(languageForName(name)).toBeDefined();
    for (const name of new Set(LANGUAGE_BY_FILENAME.values())) expect(languageForName(name)).toBeDefined();
    expect(languageForName("dotenv")).toBeDefined();
    for (const language of SELECTABLE_LANGUAGES) expect(languageForName(language.name)).toBeDefined();
    // Plain text is the one name on offer that is deliberately not a grammar.
    expect(SELECTABLE_LANGUAGES.map((language) => language.name)).not.toContain(PLAIN_TEXT);
  });

  it("labels a grammar by its written name, and title-cases the ones it has no name for", () => {
    expect(languageLabel("csharp")).toBe("C#");
    expect(languageLabel("shellscript")).toBe("Shell");
    expect(languageLabel("common-lisp")).toBe("Common Lisp");
    expect(languageLabel("mermaid")).toBe("Mermaid");
  });

  it("lists the extensions that already ask for a grammar, so the search finds it by suffix", () => {
    expect(languageExtensions("yaml")).toEqual(["yaml", "yml"]);
    expect(languageExtensions("dart")).toEqual(["dart"]);
    expect(languageExtensions("markdown")).toEqual(["md", "mdown", "markdown"]);
    expect(languageExtensions(PLAIN_TEXT)).toEqual([]);
  });
});
