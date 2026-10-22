# tree-sitter-gherkin

A [tree-sitter](https://tree-sitter.github.io/tree-sitter/) grammar for Gherkin (Cucumber) `.feature` files: `Feature`, `Rule`, `Background`, `Scenario`, `Scenario Outline`, `Examples`, steps (`Given`/`When`/`Then`/`And`/`But`/`*`), `@tags`, `# comments`, doc strings (`"""..."""`), and data tables (`| ... |`). English keywords only.

Built for and consumed by [cucumber-zed](https://github.com/DevAbdullahUk/cucumber-zed), a Zed editor extension for Gherkin syntax highlighting.

## Development

```bash
npm install
npm run generate   # regenerate the parser from grammar.js
npm test            # run the corpus tests in test/corpus/
```

To check parsing or a query file manually against the combined fixture:

```bash
./node_modules/.bin/tree-sitter parse test/fixtures/sample.feature
```

A parse with any `ERROR` node, or output that changes between repeated runs on the same file, means an ambiguity has crept into the grammar — see the comments on `rule_header` and `examples` in `grammar.js` for the specific pattern to avoid (giving two sibling rules in the same `repeat()` an overlapping optional `tag_list` prefix breaks LR(1) parsing and produces GLR conflicts that don't always resolve the same way twice).

## License

MIT — see [LICENSE](LICENSE)
