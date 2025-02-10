use std::{
    env,
    fs::{self},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
};

use xz2::write::XzEncoder;

fn main() {
    build_wasm();
    build_fs();
}

const PREBUILT_ENV_VAR: &'static str = "CEALN_RUNTIME_PYTHON_PREBUILT";
const STDLIB_ENV_VAR: &'static str = "CEALN_RUNTIME_PYTHON_STDLIB";

fn build_wasm() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR environment variable"));

    println!("cargo:rerun-if-env-changed={}", PREBUILT_ENV_VAR);
    let runtime_image_path = if let Some(path) = env::var_os(PREBUILT_ENV_VAR) {
        PathBuf::from(path)
    } else {
        // Might be a check build, just do nothing
        fs::write(out_dir.join("runtime-python.wasm"), &[]).unwrap();
        return;
    };

    println!("cargo:rerun-if-changed={}", runtime_image_path.display());

    // Copy output binary to fixed path so it can be included
    fs::copy(runtime_image_path, out_dir.join("runtime-python.wasm")).unwrap();
}

fn build_fs() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR environment variable"));
    let is_release = env::var("PROFILE").unwrap() == "release";

    let archive_path = out_dir.join("python_fs.tar.xz");

    let libs_src = if let Some(path) = env::var_os(STDLIB_ENV_VAR) {
        PathBuf::from(path)
    } else {
        // Might be a check build, just do nothing
        fs::write(&archive_path, &[]).unwrap();
        return;
    };

    let encoder = XzEncoder::new(
        BufWriter::new(fs::File::create(&archive_path).unwrap()),
        if is_release { 9 } else { 3 },
    );

    let mut builder = tar::Builder::new(encoder);

    // Write python standard libraries
    write_to_archive(&mut builder, &libs_src, Path::new("usr/lib/python3.11"));

    // Write cealn python library
    let mut pythonlib_cealn_path = std::env::current_dir().unwrap();
    pythonlib_cealn_path.pop();
    pythonlib_cealn_path.pop();
    pythonlib_cealn_path.push("pythonlib/src/cealn");
    write_to_archive(
        &mut builder,
        &pythonlib_cealn_path,
        Path::new("usr/lib/python3.11/site-packages/cealn"),
    );

    // Flush everything
    builder.into_inner().unwrap().finish().unwrap().into_inner().unwrap();
}

fn write_to_archive<W: io::Write>(builder: &mut tar::Builder<W>, source_dir: &Path, dest_dir: &Path) {
    println!("cargo:rerun-if-changed={}", source_dir.display());
    let mut directory_header = tar::Header::new_gnu();
    directory_header.set_entry_type(tar::EntryType::Directory);
    directory_header.set_path(dest_dir).unwrap();
    directory_header.set_mode(0o555);
    directory_header.set_size(0);
    directory_header.set_cksum();
    builder.append(&directory_header, &[] as &[u8]).unwrap();

    for entry in fs::read_dir(source_dir).unwrap() {
        let entry = entry.unwrap();

        if entry.file_type().unwrap().is_dir() {
            write_to_archive(builder, &entry.path(), &dest_dir.join(entry.file_name()));
        } else {
            println!("cargo:rerun-if-changed={}", entry.path().display());
            // TODO: precompile optimized pyc in release mode
            let mut file_header = tar::Header::new_gnu();
            file_header.set_entry_type(tar::EntryType::Regular);
            file_header.set_path(dest_dir.join(entry.file_name())).unwrap();
            file_header.set_mode(0o444);
            file_header.set_size(entry.metadata().unwrap().len());
            file_header.set_cksum();
            let reader = BufReader::new(fs::File::open(entry.path()).unwrap());
            builder.append(&file_header, reader).unwrap();
        }
    }
}
