use std::{env, path::Path};

fn main() {
    let mut build = cc::Build::new();
    build.std("c17");
    if build.get_compiler().is_like_msvc() {
        build.flag("-utf-8");
    }
    if env::var("TARGET").expect("Cargo must provide TARGET") == "wasm32-unknown-unknown" {
        let headers = env::var_os("DEP_TREE_SITTER_LANGUAGE_WASM_HEADERS")
            .expect("tree-sitter-language must provide WebAssembly headers");
        build.include(&headers);
        println!("cargo::rerun-if-changed={}", Path::new(&headers).display());
    }

    println!("cargo::rerun-if-changed=src");
    build
        .include("src")
        .files(["src/parser.c", "src/scanner.c"])
        .compile("tree-sitter-gitignore");
}
