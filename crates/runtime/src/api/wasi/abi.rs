use std::{io, mem};

use byteorder::{LittleEndian, WriteBytesExt};
use wiggle::GuestType;

use super::types;

pub trait ToGuestBytes<'a>: GuestType<'a> {
    fn write_guest_bytes<W: io::Write>(&self, w: &mut W) -> io::Result<usize>;

    fn to_guest_bytes(&self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::with_capacity(Self::guest_size() as usize);
        self.write_guest_bytes(&mut buffer).expect("infallible write");
        buffer
    }
}

impl ToGuestBytes<'_> for types::Dirent {
    fn write_guest_bytes<W: io::Write>(&self, w: &mut W) -> io::Result<usize> {
        w.write_u64::<LittleEndian>(self.d_next)?;
        w.write_u64::<LittleEndian>(self.d_ino)?;
        w.write_u32::<LittleEndian>(self.d_namlen)?;
        w.write_u8(unsafe { mem::transmute(self.d_type) })?;
        w.write(&[0u8; 3])?;

        Ok(Self::guest_size() as usize)
    }
}
