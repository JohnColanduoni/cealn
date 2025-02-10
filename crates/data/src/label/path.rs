use std::{
    borrow::{Borrow, Cow},
    fmt,
    ops::Deref,
    path::Path,
};

use serde::{de::Error as _, ser::SerializeMap, Deserialize, Serialize};

use crate::{
    label::{valid_filename_char, ParseError},
    LabelBuf,
};

/// Label paths implement the invidiual paths that make up a label
///
/// In particular, they can be found within the following:
///
/// * The path from the workspace root to the package
/// * The path from the package to the target or source file
/// * The path within the action output
///
/// This means they have some important invariants:
/// * They are always relative (i.e. they never have leading slashes)
#[derive(PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct LabelPath(str);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct LabelPathBuf(String);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NormalizedDescending<T>(T);

impl LabelPath {
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> Result<&LabelPath, ParseError> {
        let s = s.as_ref();
        let mut prev_char = None;
        for c in s.chars() {
            match c {
                '/' => match prev_char {
                    Some('/') => return Err(ParseError::InvalidSlashSeparator),
                    None => return Err(ParseError::InvalidSlashStart),
                    _ => {}
                },
                // FIXME: we kind of need to allow these in depmap paths since you'll find colons in paths in e.g.
                // sysroots, but these have significance in labels so I don't know how good an idea this is; the files
                // will be difficult to address.
                ':' => {}
                c if valid_filename_char(c) => {}
                c => return Err(ParseError::InvalidCharacter(c)),
            }
            prev_char = Some(c);
        }
        Ok(Self::from_str_unchecked(s))
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub(super) const fn from_str_unchecked(s: &str) -> &LabelPath {
        unsafe { &*(s as *const str as *const LabelPath) }
    }

    pub fn file_name(&self) -> Option<&LabelPath> {
        let mut segments = self.segments();
        loop {
            match segments.next_back() {
                Some(x) if x.as_str() == "." => continue,
                Some(x) if x.as_str() == ".." => return None,
                Some(value) => return Some(value),
                None => return None,
            }
        }
    }

    pub fn file_name_normalized(&self) -> Option<NormalizedDescending<&LabelPath>> {
        let mut segments = self.segments();
        loop {
            match segments.next_back() {
                Some(x) if x.as_str() == "." => continue,
                Some(x) if x.as_str() == ".." => return None,
                Some(value) => return Some(NormalizedDescending(value)),
                None => return None,
            }
        }
    }

    pub fn parent(&self) -> Option<&LabelPath> {
        let mut segments = self.segments();
        let Some(_) = segments.next_back() else {
            return None;
        };
        if !segments.as_path().as_str().is_empty() {
            Some(segments.as_path())
        } else {
            None
        }
    }

    pub fn normalize_require_descending(&self) -> Option<NormalizedDescending<Cow<LabelPath>>> {
        let baseline = if let Some(stripped) = self.0.strip_suffix('/') {
            // We allow a single trailing slash, but not for a normalized path
            Self::from_str_unchecked(stripped)
        } else {
            self
        };
        for segment in baseline.segments() {
            if segment.as_str() == "." || segment.as_str() == ".." {
                // We need to rewrite the string, nevermind
                let mut accum = String::with_capacity(self.0.len());
                for segment in baseline.segments() {
                    if segment.as_str() == "." {
                        continue;
                    } else if segment.as_str() == ".." {
                        todo!()
                    } else {
                        if !accum.is_empty() {
                            accum.push('/');
                        }
                        accum.push_str(segment.as_str());
                    }
                }
                return Some(NormalizedDescending(Cow::Owned(LabelPathBuf(accum))));
            }
        }
        Some(NormalizedDescending(Cow::Borrowed(baseline)))
    }

    pub fn require_normalized_descending(&self) -> Option<NormalizedDescending<&LabelPath>> {
        if self.0.ends_with('/') {
            // We allow a single trailing slash, but not for a normalized path
            return None;
        }
        for segment in self.segments() {
            if segment.as_str() == "." || segment.as_str() == ".." {
                return None;
            }
        }
        Some(NormalizedDescending(self))
    }

    pub fn strip_prefix<'a>(&'a self, prefix: &'_ LabelPath) -> Option<&'a LabelPath> {
        let Some(tail) = self.as_str().strip_prefix(prefix.as_str()) else {
            return None;
        };
        if tail.starts_with('/') {
            Some(LabelPath::from_str_unchecked(&tail[1..]))
        } else if prefix.as_str().ends_with('/') || prefix.as_str().is_empty() {
            Some(LabelPath::from_str_unchecked(tail))
        } else if tail.is_empty() {
            Some(LabelPath::from_str_unchecked(""))
        } else {
            None
        }
    }

    pub fn join(&self, rhs: &LabelPath) -> LabelPathBuf {
        if self.0.ends_with('/') || self.0.is_empty() || rhs.0.is_empty() {
            LabelPathBuf(format!("{}{}", &self.0, &rhs.0))
        } else {
            LabelPathBuf(format!("{}/{}", &self.0, &rhs.0))
        }
    }

    pub fn to_native_relative_path(&self) -> Cow<Path> {
        if cfg!(target_os = "windows") {
            todo!()
        } else {
            Cow::Borrowed(Path::new(self.as_str()))
        }
    }

    pub fn segments(&self) -> PathSegmentIter {
        PathSegmentIter { iter: self.0.chars() }
    }
}

impl LabelPathBuf {
    #[inline]
    pub fn new<S: Into<String>>(s: S) -> Result<LabelPathBuf, ParseError> {
        let s = s.into();
        // Create zero-copy &Label just to check if the label is valid
        let _label = LabelPath::new(&s)?;

        Ok(LabelPathBuf(s))
    }

    #[inline]
    pub fn as_ref(&self) -> &LabelPath {
        LabelPath::from_str_unchecked(&*self.0)
    }

    pub fn pop(&mut self) -> bool {
        let Some((head, _)) = self.0.rsplit_once('/') else {
            return false;
        };
        self.0.truncate(head.len());
        true
    }

    #[inline]
    pub(super) fn from_string_unchecked(string: String) -> LabelPathBuf {
        LabelPathBuf(string)
    }
}

pub struct PathSegmentIter<'a> {
    iter: std::str::Chars<'a>,
}

impl<'a> PathSegmentIter<'a> {
    pub fn as_path(&self) -> &'a LabelPath {
        LabelPath::from_str_unchecked(self.iter.as_str())
    }
}

impl<'a> Iterator for PathSegmentIter<'a> {
    type Item = &'a LabelPath;

    fn next(&mut self) -> Option<Self::Item> {
        let mut lookahead = self.iter.clone();
        loop {
            match lookahead.next() {
                Some('/') => {
                    let segment = &self.iter.as_str()[..(self.iter.as_str().len() - lookahead.as_str().len() - 1)];
                    debug_assert!(!segment.is_empty());
                    self.iter = lookahead;
                    return Some(LabelPath::from_str_unchecked(segment));
                }
                None => {
                    let segment = self.iter.as_str();
                    self.iter = lookahead;
                    if !segment.is_empty() {
                        return Some(LabelPath::from_str_unchecked(segment));
                    } else {
                        // Can happen with trailing slash
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
}

impl<'a> DoubleEndedIterator for PathSegmentIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let mut lookahead = self.iter.clone();
        loop {
            match lookahead.next_back() {
                Some('/') => {
                    let segment = &self.iter.as_str()[(lookahead.as_str().len() + 1)..];
                    if !segment.is_empty() {
                        self.iter = lookahead;
                        return Some(LabelPath::from_str_unchecked(segment));
                    } else {
                        // Can happen with trailing slash
                        self.iter = lookahead.clone();
                        continue;
                    }
                }
                None => {
                    let segment = self.iter.as_str();
                    self.iter = lookahead;
                    if !segment.is_empty() {
                        return Some(LabelPath::from_str_unchecked(segment));
                    } else {
                        // Can happen with trailing slash
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
}

impl Deref for LabelPathBuf {
    type Target = LabelPath;

    #[inline]
    fn deref(&self) -> &LabelPath {
        LabelPath::from_str_unchecked(&*self.0)
    }
}

impl Borrow<LabelPath> for LabelPathBuf {
    #[inline]
    fn borrow(&self) -> &LabelPath {
        self.as_ref()
    }
}

impl Borrow<LabelPath> for NormalizedDescending<LabelPathBuf> {
    #[inline]
    fn borrow(&self) -> &LabelPath {
        self.as_ref().into_inner()
    }
}

impl Borrow<str> for LabelPathBuf {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl ToOwned for LabelPath {
    type Owned = LabelPathBuf;

    #[inline]
    fn to_owned(&self) -> Self::Owned {
        LabelPathBuf(self.0.to_owned())
    }
}

impl AsRef<Path> for LabelPath {
    #[inline]
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl<'a> AsRef<Path> for NormalizedDescending<&'a LabelPath> {
    #[inline]
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl<T> NormalizedDescending<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<'a> NormalizedDescending<&'a LabelPath> {
    #[inline]
    pub fn strip_prefix(&'_ self, prefix: &'_ LabelPath) -> Option<NormalizedDescending<&'a LabelPath>> {
        self.0.strip_prefix(prefix).map(NormalizedDescending)
    }

    #[inline]
    pub fn join(&self, rhs: NormalizedDescending<&LabelPath>) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.join(rhs.0))
    }

    pub fn parent(&self) -> Option<NormalizedDescending<&'a LabelPath>> {
        self.0.parent().map(NormalizedDescending)
    }
}

impl NormalizedDescending<LabelPathBuf> {
    #[inline]
    pub fn strip_prefix<'b>(&'b self, prefix: &'_ LabelPath) -> Option<NormalizedDescending<&'b LabelPath>> {
        self.0.strip_prefix(prefix).map(NormalizedDescending)
    }

    #[inline]
    pub fn join(&self, rhs: NormalizedDescending<&LabelPath>) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.join(rhs.0))
    }

    #[inline]
    pub fn into_cow(self) -> NormalizedDescending<Cow<'static, LabelPath>> {
        NormalizedDescending(Cow::Owned(self.0))
    }
}

impl<'a> NormalizedDescending<Cow<'a, LabelPath>> {
    #[inline]
    pub fn as_ref(&self) -> NormalizedDescending<&LabelPath> {
        NormalizedDescending(&*self.0)
    }

    #[inline]
    pub fn strip_prefix<'b>(&'b self, prefix: &'_ LabelPath) -> Option<NormalizedDescending<&'b LabelPath>> {
        self.0.strip_prefix(prefix).map(NormalizedDescending)
    }

    #[inline]
    pub fn join(&self, rhs: NormalizedDescending<&LabelPath>) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.join(rhs.0))
    }
}

impl<'a> NormalizedDescending<&'a LabelPath> {
    #[inline]
    pub fn to_owned(&self) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.to_owned())
    }

    #[inline]
    pub fn as_cow(&self) -> NormalizedDescending<Cow<'a, LabelPath>> {
        NormalizedDescending(Cow::Borrowed(self.0))
    }
}

impl<'a> NormalizedDescending<Cow<'a, LabelPath>> {
    #[inline]
    pub fn to_owned(&self) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.clone().into_owned())
    }

    #[inline]
    pub fn into_owned(self) -> NormalizedDescending<LabelPathBuf> {
        NormalizedDescending(self.0.into_owned())
    }
}

impl NormalizedDescending<LabelPathBuf> {
    #[inline]
    pub fn as_ref<'a>(&'a self) -> NormalizedDescending<&'a LabelPath> {
        NormalizedDescending(self.0.as_ref())
    }
}

impl<T> Deref for NormalizedDescending<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

pub(crate) const LABEL_PATH_SENTINEL: &str = "$cealn_label_path";

impl Serialize for LabelPath {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(LABEL_PATH_SENTINEL, self.as_str())?;
        map.end()
    }
}

impl Serialize for LabelPathBuf {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <LabelPath as Serialize>::serialize(&*self, serializer)
    }
}

impl<'de> Deserialize<'de> for LabelPathBuf {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(LabelPathBufDeVisitor {})
    }
}

struct LabelPathBufDeVisitor {}

impl<'de> serde::de::Visitor<'de> for LabelPathBufDeVisitor {
    type Value = LabelPathBuf;

    #[inline]
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "label with sentinel")
    }

    #[inline]
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let key: Cow<str> = map
            .next_key()?
            .ok_or_else(|| A::Error::custom(format!("expected object with key {:?}", LABEL_PATH_SENTINEL)))?;
        if key != LABEL_PATH_SENTINEL {
            return Err(A::Error::custom(format!(
                "expected object with key {:?}",
                LABEL_PATH_SENTINEL
            )));
        }
        let value: String = map.next_value()?;
        // TODO: get rid of clone
        let value = LabelPathBuf::new(value.clone())
            .map_err(|err| A::Error::custom(format!("invalid label {:?}: {}", value, err)))?;
        Ok(value)
    }
}

impl Serialize for NormalizedDescending<LabelPathBuf> {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NormalizedDescending<LabelPathBuf> {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let path = LabelPathBuf::deserialize(deserializer)?;
        path.require_normalized_descending()
            .ok_or_else(|| D::Error::custom(format!("expected normalized descending path, but got {:?}", path)))?;
        Ok(NormalizedDescending(path))
    }
}

impl fmt::Display for LabelPath {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for LabelPathBuf {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<T> fmt::Display for NormalizedDescending<T>
where
    T: fmt::Display,
{
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}
