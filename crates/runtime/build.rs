use std::env;

fn main() {
    let cwd = env::current_dir().unwrap();
    println!("cargo:rustc-env=WASI_ROOT={}", cwd.display());
}
