use std::{
    fs::{self},
    io::{self, IoSliceMut, SeekFrom},
    path::{Path, PathBuf},
};

use cealn_core::fs::FilenameSemantics;
use cealn_runtime::api::{types, Handle};
use cealn_test_util::fs_test;

use super::SystemFs;

macro_rules! assert_wasi_fail {
    ($call:expr, $expected:expr) => {
        match $call {
            Ok(_) => panic!("expected {} to fail", stringify!($call)),
            Err(err) => {
                if err != $expected {
                    panic!(
                        "expected {} to fail with {:?}, but got {:?}",
                        stringify!($call),
                        $expected,
                        err
                    );
                }
            }
        }
    };
}

#[fs_test]
#[test]
fn create_empty(_semantics: FilenameSemantics, root: PathBuf) {
    let _fs = SystemFs::new(root).unwrap();
}

#[fs_test]
#[test]
fn open_file_different_ascii_case(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_File.txt"), "").unwrap();

    let root_handle = fs.root();

    assert_wasi_fail!(
        root_handle.openat_child(
            "my_file.txt",
            true,
            false,
            types::Oflags::empty(),
            types::Fdflags::empty(),
        ),
        types::Errno::Noent
    );
}

#[fs_test]
#[test]
fn open_file_reserved_win32(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    let mut verbatim_path = fs::canonicalize(&root).unwrap();
    verbatim_path.push("COM1");
    fs::write(&verbatim_path, "").unwrap();

    let root_handle = fs.root();

    root_handle
        .openat_child("COM1", true, false, types::Oflags::empty(), types::Fdflags::empty())
        .unwrap();
}

#[fs_test]
#[test]
fn read_file(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();

    let root_handle = fs.root();
    let file_handle = root_handle
        .openat_child(
            "my_file.txt",
            true,
            false,
            types::Oflags::empty(),
            types::Fdflags::empty(),
        )
        .unwrap();

    let mut buffer = [0u8; 1024];
    let bytes_read = file_handle.read(&mut [IoSliceMut::new(&mut buffer)]).unwrap();
    assert_eq!(&buffer[0..bytes_read], b"sometext")
}

#[fs_test]
#[test]
fn seek_current(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();

    let root_handle = fs.root();
    let file_handle = root_handle
        .openat_child(
            "my_file.txt",
            true,
            false,
            types::Oflags::empty(),
            types::Fdflags::empty(),
        )
        .unwrap();

    let mut buffer = [0u8; 1];
    let bytes_read = file_handle.read(&mut [IoSliceMut::new(&mut buffer)]).unwrap();
    assert_eq!(&buffer[0..bytes_read], b"s");

    file_handle.seek(SeekFrom::Current(4)).unwrap();

    let mut buffer = [0u8; 1024];
    let bytes_read = file_handle.read(&mut [IoSliceMut::new(&mut buffer)]).unwrap();
    assert_eq!(&buffer[0..bytes_read], b"ext")
}

#[fs_test]
#[test]
fn seek_start(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();

    let root_handle = fs.root();
    let file_handle = root_handle
        .openat_child(
            "my_file.txt",
            true,
            false,
            types::Oflags::empty(),
            types::Fdflags::empty(),
        )
        .unwrap();

    file_handle.seek(SeekFrom::Start(3)).unwrap();

    let mut buffer = [0u8; 1024];
    let bytes_read = file_handle.read(&mut [IoSliceMut::new(&mut buffer)]).unwrap();
    assert_eq!(&buffer[0..bytes_read], b"etext")
}

#[fs_test]
#[test]
fn seek_end(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();

    let root_handle = fs.root();
    let file_handle = root_handle
        .openat_child(
            "my_file.txt",
            true,
            false,
            types::Oflags::empty(),
            types::Fdflags::empty(),
        )
        .unwrap();

    file_handle.seek(SeekFrom::End(-3)).unwrap();

    let mut buffer = [0u8; 1024];
    let bytes_read = file_handle.read(&mut [IoSliceMut::new(&mut buffer)]).unwrap();
    assert_eq!(&buffer[0..bytes_read], b"ext")
}

#[fs_test]
#[test]
fn readlinkat_file(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();
    symlink_file(Path::new("my_file.txt"), &root.join("my_link.txt")).unwrap();

    let root_handle = fs.root();
    let link_contents = root_handle.readlinkat_child("my_link.txt").unwrap();

    assert_eq!(link_contents, "my_file.txt")
}

#[fs_test]
#[test]
fn readlinkat_regular_file(_semantics: FilenameSemantics, root: PathBuf) {
    let fs = SystemFs::new(root.clone()).unwrap();

    fs::write(root.join("my_file.txt"), "sometext").unwrap();

    let root_handle = fs.root();
    assert_wasi_fail!(root_handle.readlinkat_child("my_file.txt"), types::Errno::Inval);
}

// TODO: test more link cases (subdirectories, absolute links, etc.)

fn symlink_file(original: &Path, link: &Path) -> io::Result<()> {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "windows")] {
            std::os::windows::fs::symlink_file(original, link)
        } else {
            std::os::unix::fs::symlink(original, link)
        }
    }
}
