pub mod grpc {
    pub use crate::grpc::file::*;
}

use std::{convert::TryFrom, mem};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::ParseError;

#[derive(Clone, Debug)]
pub enum SystemFilename {
    Posix(Vec<u8>),
    Nt(Vec<u16>),
}

#[derive(Clone, Debug)]
pub enum FileMeta {
    Posix(PosixFile),
    Nt(NtFile),
}

/// Metadata for a file on a Posix-ish filesystem
#[derive(Clone, Debug)]
pub struct PosixFile {
    filename: Vec<u8>,
    writable: bool,
    executable: bool,
}

/// Metadata for a file on a NTFS-ish filesystem
#[derive(Clone, Debug)]
pub struct NtFile {
    filename: Vec<u16>,
    writable: bool,
}

impl From<SystemFilename> for grpc::SystemFilename {
    fn from(msg: SystemFilename) -> grpc::SystemFilename {
        grpc::SystemFilename {
            raw: Some(match msg {
                SystemFilename::Posix(bytes) => grpc::system_filename::Raw::Posix(grpc::PosixFilename { raw: bytes }),
                SystemFilename::Nt(code_units) => grpc::system_filename::Raw::Nt(grpc::NtFilename {
                    raw_le: {
                        let mut encoded_buffer = Vec::with_capacity(code_units.len() * mem::size_of::<u16>());
                        for &code_unit in code_units.iter() {
                            encoded_buffer.write_u16::<LittleEndian>(code_unit).unwrap();
                        }
                        encoded_buffer
                    },
                }),
            }),
        }
    }
}

impl TryFrom<grpc::SystemFilename> for SystemFilename {
    type Error = ParseError;

    fn try_from(value: grpc::SystemFilename) -> Result<Self, ParseError> {
        Ok(match value.raw.ok_or(ParseError::MissingField("raw"))? {
            grpc::system_filename::Raw::Posix(grpc::PosixFilename { raw }) => SystemFilename::Posix(raw),
            grpc::system_filename::Raw::Nt(grpc::NtFilename { raw_le }) => SystemFilename::Nt({
                if raw_le.len() % mem::size_of::<u16>() != 0 {
                    return Err(ParseError::InvalidNtFilename);
                }
                let mut decoded_buffer: Vec<u16> = Vec::with_capacity(raw_le.len() / mem::size_of::<u16>());
                let mut reader = raw_le.as_slice();
                while !reader.is_empty() {
                    let code_unit = reader.read_u16::<LittleEndian>().unwrap();
                    decoded_buffer.push(code_unit);
                }
                decoded_buffer
            }),
        })
    }
}
