use std::{env, path::PathBuf};

fn main() {
    if !cfg!(target_os = "macos") {
        return;
    }

    println!("cargo:rustc-link-lib=framework=Hypervisor");

    println!("cargo:rerun-if-changed=wrapper.h");
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg("-F/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks")
        .allowlist_function("hv_.*")
        .allowlist_function("mach_.*")
        .allowlist_var("HV.*")
        .allowlist_var("VM.*")
        .allowlist_var("IRQ.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
