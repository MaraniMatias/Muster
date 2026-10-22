# Vendored copy

Source: `DevAbdullahUk/tree-sitter-gherkin`, upstream commit
`ceeb7c88c4c911d9b6fdfc2e29374c63a2ab8927`, source archive SHA-256
`7fbdc4a871fb2de855d043f00c0b7414241c2c0c5ca5bfe7a237cd14e57dc5d5`.

The parser is upstream's, unchanged: `src/parser.c` is what the upstream
repository's own `tree-sitter generate` wrote, and the grammar it was generated
from is vendored beside it as `grammar.js`. The upstream `LICENSE` and
`README.md` are here too.

The upstream repository ships no Rust binding, so the one here is written for
this workspace: `bindings/rust/lib.rs` exposes the grammar through
`tree-sitter-language`'s `LanguageFn`, the way the workspace's other vendored
grammar does, and `bindings/rust/build.rs` compiles the parser with the `cc`
release the workspace already selects. The upstream tree's Node, Python, Go
and Swift bindings, its npm manifest and its corpus test are not vendored: none
of them is read by this build.

Everything the grammar needs to compile is here, and the whole directory is
hash-pinned by `scripts/patched-gherkin.mjs`.
