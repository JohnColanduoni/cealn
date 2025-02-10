use std::{env, fs, path::PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap();

    if target.contains("darwin") {
        build_macos_guest();
    } else if target.contains("linux") {
        build_linux_interceptor();
    }
}

const MACOS_PREBUILD_ENV_VAR: &'static str = "CEALN_EXECUTE_GUEST";

fn build_macos_guest() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR environment variable"));

    println!("cargo:rerun-if-env-changed={}", MACOS_PREBUILD_ENV_VAR);
    let runtime_image_path = if let Some(path) = env::var_os(MACOS_PREBUILD_ENV_VAR) {
        PathBuf::from(path)
    } else {
        // Might be a check build, just do nothing
        fs::write(out_dir.join("guest"), &[]).unwrap();
        return;
    };

    println!("cargo:rerun-if-changed={}", runtime_image_path.display());

    // Copy output binary to fixed path so it can be included
    fs::copy(runtime_image_path, out_dir.join("guest")).unwrap();
}

const LINUX_PREBUILD_ENV_VAR: &'static str = "CEALN_EXECUTE_INTERCEPTOR";

fn build_linux_interceptor() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR environment variable"));

    println!("cargo:rerun-if-env-changed={}", LINUX_PREBUILD_ENV_VAR);
    let runtime_image_path = if let Some(path) = env::var_os(LINUX_PREBUILD_ENV_VAR) {
        PathBuf::from(path)
    } else {
        // Might be a check build, just do nothing
        fs::write(out_dir.join("libcealn_interceptor.so"), &[]).unwrap();
        return;
    };

    println!("cargo:rerun-if-changed={}", runtime_image_path.display());

    // Copy output binary to fixed path so it can be included
    fs::copy(runtime_image_path, out_dir.join("libcealn_interceptor.so")).unwrap();
}
