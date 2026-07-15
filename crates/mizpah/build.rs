use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let index = manifest_dir.join("static").join("index.html");

    println!("cargo:rerun-if-changed=static/index.html");
    println!("cargo:rerun-if-changed=static");

    if !index.is_file() {
        panic!(
            "\n\n\
             Missing UI assets at {}.\n\
             Build the web UI first, then rebuild:\n\
               cd web && npm ci && npm run build\n\
             Or from the repo root:\n\
               just ui\n\
               just install\n",
            index.display()
        );
    }
}
