use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    if is_rust_analyzer() {
        // Rust analayzer doens't need any of these files
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    println!("cargo:rerun-if-changed=build.py");

    let result = Command::new("python3")
        .arg("build.py")
        .status()
        .expect("failed to launch build script");

    if !result.success() {
        println!("command exited with status {}", result);
        panic!("command exited with status {}", result);
    }

    println!("build script succeeded");

    let libname = if env::var("PROFILE").unwrap() == "debug" {
        "python3.11d"
    } else {
        "python3.11"
    };

    println!(
        "cargo:rustc-link-search=native={}",
        out_dir.join("python_install").to_str().unwrap()
    );
    println!("cargo:rustc-link-lib=static:-bundle={}", libname);

    if env::var("TARGET").unwrap().contains("apple") {
        // FIXME: hack for build on my machine, due to gettext being installed
        println!("cargo:rustc-link-lib=dylib=intl");
    }

    let target_dir = out_dir.parent().unwrap().parent().unwrap().parent().unwrap().to_owned();

    let libs_dest_dir = target_dir.join("python_libs");

    match fs::remove_dir_all(&libs_dest_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => panic!("{:?}", err),
    }

    copy_libs(&out_dir.join("python_install/lib/python3.11"), &libs_dest_dir);

    println!("build finished");
}

fn copy_libs(source_dir: &Path, dest_dir: &Path) {
    fs::create_dir_all(dest_dir).unwrap();

    for entry in fs::read_dir(source_dir).unwrap() {
        let entry = entry.unwrap();

        if entry.file_name() == "__pycache__" || entry.file_name() == "test" {
            continue;
        }

        if entry.file_type().unwrap().is_dir() {
            copy_libs(&entry.path(), &dest_dir.join(entry.file_name()));
        } else {
            if entry.path().extension().map(|x| x == "py") != Some(true) {
                continue;
            }

            fs::copy(entry.path(), dest_dir.join(entry.file_name())).unwrap();
        }
    }
}

fn is_rust_analyzer() -> bool {
    if let Some(wrapper) = env::var_os("RUSTC_WRAPPER") {
        let wrapper = PathBuf::from(wrapper);
        wrapper.file_name().map(|x| x == "rust-analyzer") == Some(true)
    } else {
        false
    }
}
