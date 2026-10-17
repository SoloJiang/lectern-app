//! Parses and checks locales/*/*.ftl, then compiles them into static tables
//! and the `keys` module (docs/design/i18n.md「编译期校验」). The app binary
//! contains no Fluent parser.

#[path = "build/codegen.rs"]
mod codegen;
#[path = "build/compile.rs"]
mod compile;

use std::path::PathBuf;
use std::{env, fs, process};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build");
    println!("cargo::rerun-if-changed=locales");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by Cargo"));
    let compiled = match compile::compile_dir(&manifest_dir.join("locales")) {
        Ok(compiled) => compiled,
        Err(errors) => {
            eprintln!("error: the Fluent resources in crates/i18n/locales are invalid:");
            for error in errors {
                eprintln!("  locales/{error}");
            }
            process::exit(1);
        }
    };
    fs::write(out_dir.join("table.rs"), codegen::table_rs(&compiled)).expect("write table.rs");
    fs::write(out_dir.join("keys.rs"), codegen::keys_rs(&compiled)).expect("write keys.rs");
}
