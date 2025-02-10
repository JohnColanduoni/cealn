use core::{convert::TryFrom, fmt, ops::Sub};

/// A pointer in a remote process
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct RemotePointer(pub u64);

#[inline]
pub fn align_addr<T: PtrLike>(addr: T, alignment: T::USize) -> T {
    if addr.modulus(alignment) == T::zero_usize() {
        addr
    } else {
        addr.add(alignment - addr.modulus(alignment))
    }
}

#[inline]
pub fn align_addr_force_up<T: PtrLike>(addr: T, alignment: T::USize) -> T {
    addr.add(alignment - addr.modulus(alignment))
}

#[inline]
pub fn align_addr_down<T: PtrLike>(addr: T, alignment: T::USize) -> T {
    if addr.modulus(alignment) == T::zero_usize() {
        addr
    } else {
        addr.sub(addr.modulus(alignment))
    }
}

#[inline]
pub fn addr_relative_offset<T: PtrLike>(to: T, from: T) -> Option<T::ISize> {
    if to > from {
        T::ISize::try_from(to.offset_from_unsigned(from)).ok()
    } else {
        T::ISize::try_from(from.offset_from_unsigned(to)).ok()
    }
}

impl RemotePointer {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for RemotePointer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#016x}", self.0)
    }
}

pub trait PtrLike: Clone + Copy + Ord {
    type USize: Clone + Copy + Sub<Output = Self::USize> + Eq;
    type ISize: Clone + Copy + Sub<Output = Self::ISize> + Eq + TryFrom<Self::USize>;

    fn add(self, offset: Self::USize) -> Self;
    fn sub(self, offset: Self::USize) -> Self;
    fn offset(self, offset: Self::ISize) -> Self;
    fn modulus(self, modulus: Self::USize) -> Self::USize;

    fn offset_from_unsigned(self, other: Self) -> Self::USize;

    fn zero_usize() -> Self::USize;
}

impl PtrLike for usize {
    type USize = usize;
    type ISize = isize;

    #[inline]
    fn modulus(self, modulus: usize) -> usize {
        self % modulus
    }

    #[inline]
    fn sub(self, offset: usize) -> Self {
        self - offset
    }

    #[inline]
    fn add(self, offset: usize) -> Self {
        self + offset
    }

    #[inline]
    fn offset(self, offset: Self::ISize) -> Self {
        if offset > 0 {
            self + offset as usize
        } else {
            self - (-offset) as usize
        }
    }

    #[inline]
    fn offset_from_unsigned(self, other: Self) -> Self::USize {
        self - other
    }

    #[inline]
    fn zero_usize() -> Self::USize {
        0
    }
}

impl PtrLike for u64 {
    type USize = u64;
    type ISize = i64;

    #[inline]
    fn modulus(self, modulus: u64) -> u64 {
        self % modulus
    }

    #[inline]
    fn sub(self, offset: u64) -> Self {
        self - offset
    }

    #[inline]
    fn add(self, offset: u64) -> Self {
        self + offset
    }

    #[inline]
    fn offset(self, offset: Self::ISize) -> Self {
        if offset > 0 {
            self + offset as u64
        } else {
            self - (-offset) as u64
        }
    }

    #[inline]
    fn offset_from_unsigned(self, other: Self) -> Self::USize {
        self - other
    }

    #[inline]
    fn zero_usize() -> Self::USize {
        0
    }
}

impl PtrLike for RemotePointer {
    type USize = u64;
    type ISize = i64;

    #[inline]
    fn modulus(self, modulus: u64) -> u64 {
        self.0 % modulus
    }

    #[inline]
    fn sub(self, offset: u64) -> Self {
        RemotePointer(self.0 - offset)
    }

    #[inline]
    fn add(self, offset: u64) -> Self {
        RemotePointer(self.0 + offset)
    }

    #[inline]
    fn offset(self, offset: Self::ISize) -> Self {
        if offset > 0 {
            RemotePointer(self.0 + offset as u64)
        } else {
            RemotePointer(self.0 - (-offset) as u64)
        }
    }

    #[inline]
    fn offset_from_unsigned(self, other: Self) -> Self::USize {
        self.0 - other.0
    }

    #[inline]
    fn zero_usize() -> Self::USize {
        0
    }
}
