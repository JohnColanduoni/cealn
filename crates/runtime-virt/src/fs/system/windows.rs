use std::{
    convert::TryFrom,
    ffi::OsString,
    fmt,
    fs::File,
    io::{self, Read, SeekFrom},
    mem,
    os::windows::prelude::{FromRawHandle, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
    ptr, slice,
    sync::Arc,
};

use cealn_core::fs::FileExt;
use cealn_runtime::api::{types, types::Errno, Handle, HandleRights, Result as WasiResult};
use tracing::error;
use widestring::U16CString;
use winapi::{
    shared::{
        minwindef::{DWORD, ULONG, USHORT},
        ntdef::{FALSE, LARGE_INTEGER, WCHAR},
        winerror::{ERROR_FILE_NOT_FOUND, ERROR_NOT_A_REPARSE_POINT},
    },
    um::{
        fileapi::{CreateFileW, ReadFile, SetFilePointerEx, FILE_ATTRIBUTE_TAG_INFO, FILE_NAME_INFO, OPEN_EXISTING},
        ioapiset::DeviceIoControl,
        minwinbase::{FileAttributeTagInfo, FileNameInfo},
        winbase::{
            GetFileInformationByHandleEx, FILE_BEGIN, FILE_CURRENT, FILE_END, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_POSIX_SEMANTICS,
        },
        winioctl::FSCTL_GET_REPARSE_POINT,
        winnt::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE, IO_REPARSE_TAG_SYMLINK, MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
        },
    },
};
use winhandle::{macros::INVALID_HANDLE_VALUE, WinHandle};

use super::{directory_rights, inherited_rights, regular_file_rights, CreateError};

pub struct DirectoryHandle {
    // This is always a verbatim ("\\?\") path, so we don't need to worry about special file names or case/normalization
    path: PathBuf,
}

pub struct RegularFileHandle {
    handle: WinHandle,
    path: PathBuf,
}

impl DirectoryHandle {
    pub(super) fn from_path(path: &Path) -> std::result::Result<Self, CreateError> {
        // Ensure we have a rooted path with a verbatim prefix ("\\?\")
        let mut components_iter = path.components();
        let verbatim_path = match components_iter.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) => path.to_owned(),
                Prefix::Disk(drive_letter) => {
                    let mut path_buf = PathBuf::from(format!(r#"\\?\{}:"#, drive_letter as char));
                    // Re-encode all path components to convert any forward slashes
                    for component in components_iter {
                        path_buf.push(component.as_os_str());
                    }
                    path_buf
                }
                Prefix::UNC(server, share) => {
                    let mut new_prefix = OsString::from(r#"\\?\UNC\"#);
                    new_prefix.push(server);
                    new_prefix.push("\\");
                    new_prefix.push(share);
                    let mut path_buf = PathBuf::from(new_prefix);
                    // Re-encode all path components to convert any forward slashes
                    for component in components_iter {
                        path_buf.push(component.as_os_str());
                    }
                    path_buf
                }
                Prefix::Verbatim(_) => return Err(CreateError::RelativeRootPath),
                Prefix::DeviceNS(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "virtual filesystem root cannot be a device path",
                    )
                    .into())
                }
            },
            Some(Component::CurDir)
            | Some(Component::ParentDir)
            | Some(Component::RootDir)
            | Some(Component::Normal(_))
            | None => return Err(CreateError::RelativeRootPath),
        };

        Ok(DirectoryHandle { path: verbatim_path })
    }

    pub(super) fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        _fd_flags: types::Fdflags,
    ) -> WasiResult<super::Handle> {
        if write
            | oflags.contains(&types::Oflags::CREAT)
            | oflags.contains(&types::Oflags::EXCL)
            | oflags.contains(&types::Oflags::TRUNC)
        {
            return Err(Errno::Notcapable);
        }

        unsafe {
            let full_path = self.path.join(path_segment);
            let wstr_path = U16CString::from_os_str(&full_path).expect("null in path");
            let handle = CreateFileW(
                wstr_path.as_ptr(),
                GENERIC_READ,
                // Don't impede any concurrent operations on the file
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                let err = io::Error::last_os_error();
                error!(
                    os_error_code = ?err.raw_os_error(),
                    "CreateFileW for openat failed: {}", err
                );
                if err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
                    return Err(Errno::Noent);
                }
                // FIXME: error for interventing directory not being a directory?
                return Err(Errno::Io);
            }
            let handle = WinHandle::from_raw_unchecked(handle);

            // Check that the filename is a byte-for-byte match, Win32 will apply case and normalization insensitivity.
            // You might hope that `FILE_FLAG_POSIX_SEMANTICS` would do this, but a global registry flag set by default
            // disables it :/
            //
            // Normally we'd have to worry about links here, but the caller guarantees that they provided a single
            // path segment and we open the file with `FILE_FLAG_OPEN_REPARSE_POINT`
            let mut name_info_buffer = [0u8; 4096];
            let ret = GetFileInformationByHandleEx(
                handle.get(),
                FileNameInfo,
                name_info_buffer.as_mut_ptr() as _,
                name_info_buffer.len() as DWORD,
            );
            if ret == 0 {
                let err = io::Error::last_os_error();
                error!(
                    os_error_code = ?err.raw_os_error(),
                    "GetFileInformationByHandleEx for FileNameInfo failed: {}", err
                );
                return Err(Errno::Io);
            }
            let name_info = &*(name_info_buffer.as_ptr() as *const FILE_NAME_INFO);
            let file_path_slice =
                slice::from_raw_parts(name_info.FileName.as_ptr(), (name_info.FileNameLength / 2) as usize);
            let last_segment_index = file_path_slice
                .iter()
                .rev()
                .enumerate()
                .find(|(_, &c)| c == (b'\\' as u16))
                .map(|(index, _)| file_path_slice.len() - index)
                .unwrap_or(0);
            let actual_segment = &file_path_slice[last_segment_index..];
            let actual_segment = OsString::from_wide(actual_segment);
            if actual_segment
                .to_str()
                .map(|segment| segment != path_segment)
                .unwrap_or(false)
            {
                error!("CreateFileW succeeded, but the file had a name with different case/normalization");
                return Err(Errno::Noent);
            }

            // Get FILE_ATTRIBUTE_TAG_INFO since it will give us all the information we want: the file attributes and
            // the reparse tag if this is a reparse point
            let mut attribute_info: FILE_ATTRIBUTE_TAG_INFO = mem::zeroed();
            let ret = GetFileInformationByHandleEx(
                handle.get(),
                FileAttributeTagInfo,
                &mut attribute_info as *mut FILE_ATTRIBUTE_TAG_INFO as _,
                mem::size_of_val(&attribute_info) as DWORD,
            );
            if ret == 0 {
                let err = io::Error::last_os_error();
                error!(
                    os_error_code = ?err.raw_os_error(),
                    "GetFileInformationByHandleEx for FileAttributeTagInfo failed: {}", err
                );
                return Err(Errno::Io);
            }

            // This field is mis-named in winapi
            let file_attributes = attribute_info.NextEntryOffset;
            if file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // Possibly a symlink
                // NOTE: must be checked before `FILE_ATTRIBUTE_DIRECTORY` because both may be set
                todo!()
            } else if file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                // Directory
                todo!()
            } else {
                if oflags.contains(&types::Oflags::DIRECTORY) {
                    return Err(Errno::Notdir);
                }

                Ok(super::Handle::RegularFile(Arc::new(super::RegularFileHandle {
                    inner: RegularFileHandle {
                        handle,
                        path: full_path,
                    },
                })))
            }
        }
    }

    pub(super) fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        todo!()
    }

    pub(super) fn readlinkat(&self, path_segment: &str) -> WasiResult<String> {
        // Pretend paths with backslashes simply don't exist, as there is no way a source checkout on Windows could
        // contain them.
        if path_segment.contains('\\') {
            return Err(Errno::Noent);
        }
        unsafe {
            // Open path with `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`
            let full_path = self.path.join(path_segment);
            let wstr_path = U16CString::from_os_str(&full_path).expect("null in path");
            let handle = CreateFileW(
                wstr_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                let err = io::Error::last_os_error();
                error!(
                    os_error_code = ?err.raw_os_error(),
                    "CreateFileW for readlinkat failed: {}", err
                );
                if err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
                    return Err(Errno::Noent);
                } else {
                    return Err(Errno::Io);
                }
            }
            let handle = WinHandle::from_raw_unchecked(handle);

            // Read reparse data using this borderline undocumented ioctl
            let mut reparse_buffer = [0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
            let mut reparse_buffer_byte_count = 0;
            let ret = DeviceIoControl(
                handle.get(),
                FSCTL_GET_REPARSE_POINT,
                ptr::null_mut(),
                0,
                reparse_buffer.as_mut_ptr() as _,
                reparse_buffer.len() as u32,
                &mut reparse_buffer_byte_count,
                ptr::null_mut(),
            );
            if ret == 0 {
                let err = io::Error::last_os_error();
                error!(
                    os_error_code = ?err.raw_os_error(),
                    "DeviceIoControl for readlinkat failed: {}", err
                );
                if err.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT as i32) {
                    // TODO: is this the best error to use here?
                    return Err(Errno::Inval);
                } else {
                    return Err(Errno::Io);
                }
            }
            let reparse = &*(reparse_buffer.as_ptr() as *const sys::REPARSE_DATA_BUFFER);
            if reparse.ReparseTag == IO_REPARSE_TAG_SYMLINK {
                let info = &reparse.Union.SymbolicLinkReparseBuffer;
                let buffer_ptr = info.PathBuffer.as_ptr();
                let subst_slice = slice::from_raw_parts(
                    buffer_ptr.add(info.SubstituteNameOffset as usize / 2),
                    info.SubstituteNameLength as usize / 2,
                );
                // Absolute paths may start with `\??\`. We won't follow absolute symlinks so we can just error out
                // early for these
                if subst_slice.starts_with(&[92u16, 63u16, 63u16, 92u16]) {
                    error!("found NT internal namespace prefix in symlink");
                    // Produce an uncacheable error
                    return Err(Errno::Io);
                }

                // Produce a clean and consistent WASI path
                let subst = PathBuf::from(OsString::from_wide(subst_slice));
                let mut cleaned_path = String::new();
                let mut first_component = true;
                for component in subst.components() {
                    if !first_component {
                        cleaned_path.push('/');
                    }
                    match component {
                        Component::Prefix(prefix) => match prefix.kind() {
                            Prefix::Verbatim(segment) => match segment.to_str() {
                                Some(segment) => {
                                    cleaned_path.push_str(segment);
                                }
                                None => {
                                    error!(path = ?subst, segment = ?segment, "found invalid UTF-16 in symlink");
                                    return Err(Errno::Io);
                                }
                            },
                            _ => {
                                error!(path = ?subst, "found absolute path in symlink");
                                return Err(Errno::Io);
                            }
                        },
                        Component::RootDir => {
                            error!(path = ?subst, "found absolute path in symlink");
                            return Err(Errno::Io);
                        }
                        Component::CurDir => {
                            cleaned_path.push('.');
                        }
                        Component::ParentDir => {
                            cleaned_path.push_str("..");
                        }
                        Component::Normal(segment) => match segment.to_str() {
                            Some(segment) => {
                                cleaned_path.push_str(segment);
                            }
                            None => {
                                error!(path = ?subst, segment = ?segment, "found invalid UTF-16 in symlink");
                                return Err(Errno::Io);
                            }
                        },
                    }
                    first_component = false;
                }
                // Match the trailing slash if present in link
                if subst.ends_with("/") || subst.ends_with("\\") {
                    cleaned_path.push('/');
                }
                Ok(cleaned_path)
            } else {
                // Not a symlink
                Err(Errno::Inval)
            }
        }
    }

    pub(super) fn filestat(&self) -> WasiResult<types::Filestat> {
        todo!()
    }

    pub(super) fn filestat_child(&self, path_segment: &str) -> WasiResult<types::Filestat> {
        todo!()
    }
}

impl RegularFileHandle {
    pub(super) fn read(&self, iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        unsafe {
            // Windows file vectored IO must be by page, so we can't generally use it

            let mut total_bytes_read = 0usize;
            for iov in iovs {
                let mut iov_bytes_read: DWORD = 0;
                let ret = ReadFile(
                    self.handle.get(),
                    iov.as_mut_ptr() as _,
                    DWORD::try_from(iov.len()).map_err(|err| Errno::Overflow)?,
                    &mut iov_bytes_read,
                    ptr::null_mut(),
                );
                if ret == 0 {
                    let err = io::Error::last_os_error();
                    error!("ReadFile failed: {}", err);
                    return Err(Errno::Io);
                }
                total_bytes_read += iov_bytes_read as usize;
                if (iov_bytes_read as usize) < iov.len() {
                    return Ok(total_bytes_read);
                }
            }

            Ok(total_bytes_read)
        }
    }

    pub(super) fn tell(&self) -> WasiResult<types::Filesize> {
        unsafe {
            let zero: LARGE_INTEGER = mem::zeroed();
            let mut new_file_pointer: LARGE_INTEGER = mem::zeroed();
            let ret = SetFilePointerEx(self.handle.get(), zero, &mut new_file_pointer, FILE_CURRENT);
            if ret == 0 {
                let err = io::Error::last_os_error();
                error!("SetFilePointerEx failed: {}", err);
                return Err(Errno::Io);
            }
            let new_file_pointer =
                ((new_file_pointer.u().HighPart as u64) << 32) | (new_file_pointer.u().LowPart as u64);
            Ok(new_file_pointer)
        }
    }

    pub(super) fn seek(&self, pos: SeekFrom) -> WasiResult<u64> {
        unsafe {
            let (method, distance) = match pos {
                SeekFrom::Current(val) => (FILE_CURRENT, to_large_integer(val)),
                SeekFrom::Start(val) => (FILE_BEGIN, to_large_integer_unsigned(val)),
                SeekFrom::End(val) => (FILE_END, to_large_integer(val)),
            };
            let mut new_file_pointer: LARGE_INTEGER = mem::zeroed();
            let ret = SetFilePointerEx(self.handle.get(), distance, &mut new_file_pointer, method);
            if ret == 0 {
                let err = io::Error::last_os_error();
                error!("SetFilePointerEx failed: {}", err);
                return Err(Errno::Io);
            }
            let new_file_pointer =
                ((new_file_pointer.u().HighPart as u64) << 32) | (new_file_pointer.u().LowPart as u64);
            Ok(new_file_pointer)
        }
    }
}

fn to_large_integer(value: i64) -> LARGE_INTEGER {
    unsafe {
        let mut lg_int: LARGE_INTEGER = mem::zeroed();
        lg_int.s_mut().LowPart = value as u32;
        lg_int.s_mut().HighPart = (value >> 32) as i32;
        lg_int
    }
}

fn to_large_integer_unsigned(value: u64) -> LARGE_INTEGER {
    unsafe {
        let mut lg_int: LARGE_INTEGER = mem::zeroed();
        lg_int.u_mut().LowPart = value as u32;
        lg_int.u_mut().HighPart = (value >> 32) as i32;
        lg_int
    }
}

impl fmt::Debug for DirectoryHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("system::DirectoryHandle")
            .field("path", &self.path)
            .finish()
    }
}

impl fmt::Debug for RegularFileHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("system::RegularFileHandle")
            .field("path", &self.path)
            .finish()
    }
}

#[allow(bad_style)]
#[allow(dead_code)]
mod sys {
    use super::*;

    #[repr(C)]
    pub struct REPARSE_DATA_BUFFER {
        pub ReparseTag: ULONG,
        pub ReparseDataLength: USHORT,
        pub Reserved: USHORT,
        pub Union: REPARSE_DATA_BUFFER_union,
    }

    #[repr(C)]
    pub union REPARSE_DATA_BUFFER_union {
        pub SymbolicLinkReparseBuffer: SYMBOLIC_LINK_REPARSE_BUFFER,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    pub struct SYMBOLIC_LINK_REPARSE_BUFFER {
        pub SubstituteNameOffset: USHORT,
        pub SubstituteNameLength: USHORT,
        pub PrintNameOffset: USHORT,
        pub PrintNameLength: USHORT,
        pub Flags: ULONG,
        pub PathBuffer: [WCHAR; 1],
    }
}
