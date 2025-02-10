use core::{cmp, fmt};

pub fn format_static<'a>(buffer: &'a mut [u8], args: fmt::Arguments) -> &'a [u8] {
    let (head, tail) = buffer.split_at_mut(0);
    let mut output = FormatStatic {
        written: head,
        remaining: tail,
    };
    let _ = fmt::Write::write_fmt(&mut output, args);
    output.written
}

struct FormatStatic<'a> {
    written: &'a mut [u8],
    remaining: &'a mut [u8],
}

impl<'a> fmt::Write for FormatStatic<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let to_copy = cmp::min(s.len(), self.remaining.len());
        self.remaining[..to_copy].copy_from_slice(&s.as_bytes()[..to_copy]);
        self.written =
            unsafe { core::slice::from_raw_parts_mut(self.written.as_mut_ptr(), self.written.len() + to_copy) };
        self.remaining = unsafe {
            core::slice::from_raw_parts_mut(self.remaining.as_mut_ptr().add(to_copy), self.remaining.len() - to_copy)
        };
        Ok(())
    }
}
