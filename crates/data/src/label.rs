mod path;

pub use self::path::{LabelPath, LabelPathBuf, NormalizedDescending};

use std::{
    borrow::{Borrow, Cow, ToOwned},
    convert::TryFrom,
    fmt::{self, Debug, Display},
    ops::Deref,
    path::{Path, PathBuf},
    str::{Chars, FromStr},
};

use serde::{de::Error as DeError, ser::SerializeMap, Deserialize, Serialize};
use thiserror::Error;

/// A label of a target, generated file, or source file
///
/// Filenames within labels consist of a sequence of Unicode characters, except the following:
///     * Filenames that consist of entirely dots (`.`, `..`, `...`, etc.)
///     * Any filename containing:
///         - `:`, `/`, `\`
///         - Unprintable ASCII characters
///
/// Note that empty labels are not allowed
// TODO: elaborate on label structure
#[repr(transparent)]
#[derive(PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(str);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LabelBuf(String);

#[repr(transparent)]
#[derive(PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct WorkspaceName(str);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorkspaceNameBuf(String);

impl Label {
    pub const ROOT: &'static Label = Label::from_str_unchecked("//");

    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> Result<&Label, ParseError> {
        let s = s.as_ref();
        if s.is_empty() {
            return Err(ParseError::InvalidStart);
        }

        // Parse enough of the string to validate it
        let (_, cont) = get_root(s)?;
        let mut segments = SegmentsChecker::new(cont);
        while let Some(result) = segments.next() {
            let _ = result?;
        }

        Ok(Label::from_str_unchecked(s.as_ref()))
    }

    pub fn len(&self) -> usize {
        self.as_str().len()
    }

    pub fn root(&self) -> Root {
        match get_root(&self.0) {
            Ok((root, _)) => root,
            Err(_err) => unreachable!(),
        }
    }

    pub fn split_root(&self) -> (Root, &Label) {
        match get_root(&self.0) {
            Ok((root, rest)) => (root, Label::from_str_unchecked(rest)),
            Err(_err) => unreachable!(),
        }
    }

    pub fn parts(&self) -> Parts {
        let mut segments = self.segments();
        let root_len = self.len() - segments.as_label().len();

        enum Mode {
            Package,
            Target,
            ActionId,
            ActionPath,
        }

        let mut mode = Mode::Package;
        let mut package = None;
        let mut target = None;
        let mut action_id = None;
        let mut action_path = None;

        let mut previous_part_end = root_len;
        loop {
            let part = match segments.next() {
                Some(Segment::CurrentDirectory) => continue,
                Some(Segment::ParentDirectory) => continue,
                Some(Segment::All) => todo!(),
                Some(Segment::Filename(filename)) => match mode {
                    Mode::ActionId => {
                        action_id = Some(filename.as_str());
                        mode = Mode::ActionPath;
                        previous_part_end = self.len() - segments.as_label().len();
                        continue;
                    }
                    _ => continue,
                },
                Some(Segment::Colon) => &self.as_str()[previous_part_end..(self.len() - segments.as_label().len() - 1)],
                None => &self.as_str()[previous_part_end..],
            };

            match mode {
                Mode::Package => {
                    if !part.is_empty() {
                        package = Some(LabelPath::from_str_unchecked(part));
                    }
                    mode = Mode::Target;
                }
                Mode::Target => {
                    if !part.is_empty() {
                        target = Some(LabelPath::from_str_unchecked(part));
                    }
                    mode = Mode::ActionId;
                }
                Mode::ActionId => {}
                Mode::ActionPath => {
                    if !part.is_empty() {
                        action_path = Some(LabelPath::from_str_unchecked(part));
                    }
                }
            }
            previous_part_end = self.len() - segments.as_label().len();

            if segments.as_label().len() == 0 {
                break;
            }
        }

        Parts {
            full: self,
            root: segments.root,
            package,
            target,
            action_id,
            action_path,
        }
    }

    pub fn split_package(&self) -> (Option<&Label>, Option<&Label>) {
        let mut segments = self.segments();
        let _without_root = segments.as_label();

        while let Some(segment) = segments.next() {
            if let Segment::Colon = segment {
                let package_str = &self.as_str()[..(self.len() - segments.as_label().len() - 1)];
                if package_str.is_empty() {
                    return (None, Some(segments.as_label()));
                } else {
                    return (Some(Label::from_str_unchecked(package_str)), Some(segments.as_label()));
                }
            }
        }

        (Some(self), None)
    }

    pub fn package(&self) -> Option<&Label> {
        let (package, _) = self.split_package();
        package
    }

    pub fn action(&self) -> Option<&Label> {
        let parts = self.parts();
        parts.full_action()
    }

    pub fn action_id(&self) -> Option<&str> {
        self.parts().action_id
    }

    pub fn action_path(&self) -> Option<&LabelPath> {
        self.parts().action_path
    }

    pub fn source_file_path(&self) -> Option<Cow<LabelPath>> {
        let parts = self.parts();
        if parts.action_id.is_some() {
            return None;
        }
        for segment in self.segments() {
            match segment {
                Segment::CurrentDirectory | Segment::ParentDirectory | Segment::Filename(_) => {}
                Segment::All => return None,
                Segment::Colon => {
                    // Need to create a copy without colon
                    let mut new_path = String::new();
                    for segment in self.segments() {
                        if !new_path.is_empty() {
                            new_path.push('/');
                        }
                        match segment {
                            Segment::CurrentDirectory => new_path.push_str("."),
                            Segment::ParentDirectory => new_path.push_str(".."),
                            Segment::All => return None,
                            Segment::Filename(filename) => new_path.push_str(filename.as_str()),
                            Segment::Colon => continue,
                        }
                    }
                    return Some(Cow::Owned(LabelPathBuf::from_string_unchecked(new_path)));
                }
            }
        }
        Some(Cow::Borrowed(LabelPath::from_str_unchecked(
            parts.package.map(|x| x.as_str()).unwrap_or(""),
        )))
    }

    pub fn is_package_relative(&self) -> bool {
        match self.root() {
            Root::PackageRelative => true,
            _ => false,
        }
    }

    pub fn is_workspace_relative(&self) -> bool {
        match self.root() {
            Root::WorkspaceRelative => true,
            _ => false,
        }
    }

    /// Checks if the `Label` points to a package, but not a target within a package (i.e. no colon)
    pub fn is_package(&self) -> bool {
        !self.is_target()
    }

    /// Checks if the `Label` points to a target within a package (i.e. contains a colon)
    pub fn is_target(&self) -> bool {
        self.as_str().contains(':')
    }

    #[inline]
    pub fn segments(&self) -> Segments {
        let (root, cont) = get_root(self.as_str()).unwrap();
        Segments {
            root,
            iter: cont.chars(),
        }
    }

    /// Attempts to eliminate all `.` and `..` entries from the path.
    ///
    /// Returns an error if this is not possible (i.e. path ascends beyond its root)
    pub fn normalize(&self) -> Result<Cow<Label>, NormalizeError> {
        let mut buffer: Option<LabelBuf> = None;
        let mut segments = self.segments();
        let mut scanned_byte_count: usize = self.len() - segments.as_label().len();
        while let Some(segment) = segments.next() {
            if let Some(buffer) = buffer.as_mut() {
                match segment {
                    Segment::Filename(name) => buffer.push(&name).unwrap(),
                    Segment::All => buffer.push(Label::from_str_unchecked("...")).unwrap(),
                    Segment::CurrentDirectory => {}
                    Segment::ParentDirectory => {
                        if !buffer.pop() {
                            return Err(NormalizeError::EscapesRoot);
                        }
                    }
                    Segment::Colon => buffer.0.push(':'),
                }
            } else {
                match segment {
                    Segment::Filename(_) | Segment::All => {
                        scanned_byte_count = self.len() - segments.as_label().len();
                    }
                    Segment::Colon => {
                        // Check for 'something/:otherthing', which normalizes to `something:otherthing`
                        let is_slash_colon = {
                            let scanned_bytes = &self.as_str()[..scanned_byte_count];
                            let mut chars = scanned_bytes.chars();
                            if let Some('/') = chars.next_back() {
                                if let Some('/') = chars.clone().next_back() {
                                    // It's a workspace label, don't trim
                                    false
                                } else {
                                    true
                                }
                            } else {
                                false
                            }
                        };
                        if is_slash_colon {
                            // We have a 'something/:otherthing', so we need normalization
                            let mut scanned_bytes = &self.as_str()[..scanned_byte_count];
                            // Trim any trailing separators, but don't trim the `//` for a workspace label
                            let mut chars = scanned_bytes.chars();
                            if let Some('/') = chars.next_back() {
                                if let Some('/') = chars.clone().next_back() {
                                    // It's a workspace label, don't trim
                                } else {
                                    scanned_bytes = chars.as_str();
                                }
                            }
                            let mut str_buffer = String::with_capacity(self.len());
                            str_buffer.push_str(scanned_bytes);
                            str_buffer.push(':');
                            let new_buffer = LabelBuf(str_buffer);
                            buffer = Some(new_buffer);
                        } else {
                            scanned_byte_count = self.len() - segments.as_label().len();
                        }
                    }
                    Segment::CurrentDirectory | Segment::ParentDirectory => {
                        let mut scanned_bytes = &self.as_str()[..scanned_byte_count];
                        // Trim any trailing separators, but don't trim the `//` for a workspace label
                        let mut chars = scanned_bytes.chars();
                        if let Some('/') = chars.next_back() {
                            if let Some('/') = chars.clone().next_back() {
                                // It's a workspace label, don't trim
                            } else {
                                scanned_bytes = chars.as_str();
                            }
                        }
                        let mut str_buffer = String::with_capacity(self.len());
                        str_buffer.push_str(scanned_bytes);
                        let mut new_buffer = LabelBuf(str_buffer);
                        if let Segment::ParentDirectory = segment {
                            if !new_buffer.pop() {
                                return Err(NormalizeError::EscapesRoot);
                            }
                        }
                        buffer = Some(new_buffer);
                    }
                }
            }
        }

        if let Some(buffer) = buffer {
            Ok(Cow::Owned(buffer))
        } else {
            Ok(Cow::Borrowed(self))
        }
    }

    pub fn from_native_relative_path<'a, P: AsRef<Path> + ?Sized + 'a>(p: &'a P) -> Result<Cow<'a, Label>, ParseError> {
        let p = p.as_ref();
        if !p.is_relative() {
            return Err(ParseError::FromNativeRelativePathsOnly);
        }

        if p.as_os_str().is_empty() {
            // We don't allow empty labels
            return Ok(Cow::Borrowed(Label::from_str_unchecked(".")));
        }

        let p_str = p.to_str().ok_or(ParseError::InvalidUnicode)?;
        // Label only allow single forward slashes as separators, and trailing slashes are not allowed
        // Colons in filenames also don't match the special purpose they have in labels, so we need to filter those
        // Similarly, duplicate slashes are generally allowed in paths but we need to filter them
        // Windows paths will have backslashes
        // FIXME: I don't think I've fully covered the filename with all periods case

        // We loop through optimisticly at first, hoping we don't need to make a copy
        let mut iter = p_str.chars();
        let mut previous_character: Option<char> = None;
        let checked_length = loop {
            let next_char = iter.next();
            match next_char {
                // This is acceptable but it means we need to make a copy
                Some('\\') if cfg!(target_os = "windows") => break p_str.len() - iter.as_str().len() - 1,
                // This is acceptable but it means we need to eliminate double slashes
                Some('/') if previous_character == Some('/') => break p_str.len() - iter.as_str().len() - 2,
                Some('/') => break p_str.len() - iter.as_str().len() - 1,
                Some(':') => return Err(ParseError::InvalidColonSeparator),
                Some(c) if valid_filename_char(c) => {}
                Some(c) => return Err(ParseError::InvalidCharacter(c)),
                // Made it through the entire string without detecting any issues, we can avoid a copy
                None => {
                    // Remove any trailing slash
                    let mut back_iter = p_str.chars();
                    let final_str = if back_iter.next_back() == Some('/') {
                        back_iter.as_str()
                    } else {
                        p_str
                    };
                    return Ok(Cow::Borrowed(unsafe { &*(final_str as *const str as *const Label) }));
                }
            }
            previous_character = next_char;
        };

        let mut buffer = String::with_capacity(p_str.len());
        let (verbatim, unprocessed) = p_str.split_at(checked_length);
        buffer.push_str(verbatim);

        // In the double-slash case, the prevous_character will be wrong otherwise. We ensure there is no trailing slash
        // in `verbatim` so it doesn't matter.
        previous_character = None;

        let mut iter = unprocessed.chars();
        loop {
            let next_char = iter.next();
            match next_char {
                Some('/') => {
                    if previous_character == Some('/') {
                        // Duplicate slash, do nothing
                    } else {
                        buffer.push('/');
                    }
                }
                Some('\\') => {
                    if previous_character == Some('\\') {
                        // Duplicate slash, do nothing
                    } else {
                        buffer.push('/');
                    }
                }
                Some(':') => return Err(ParseError::InvalidColonSeparator),
                Some(c) if valid_filename_char(c) => buffer.push(c),
                Some(c) => return Err(ParseError::InvalidCharacter(c)),
                None => {
                    // Remove any trailing slash
                    let mut back_iter = buffer.chars();
                    if back_iter.next_back() == Some('/') {
                        buffer.pop();
                    }
                    return Ok(Cow::Owned(LabelBuf(buffer)));
                }
            }
            previous_character = next_char;
        }
    }

    /// Takes a package-relative `Label` and converts it to a corresponding relative native [`Path`][Path].
    ///
    /// Returns an error if the label is not package relative or contains an "all" (`...`) segment.
    ///
    /// This function must return a [`Cow<Path>`][Cow] because
    ///     (1) Colons must be turned into path separators
    ///     (2) Some operating systems (e.g. Windows) require changes
    ///
    // NOTE: The Windows case is needed becasue of the filename separator, not the encoding: Rust uses WTF-8, which can
    // handle our UTF-8 paths just fine. Windows generally accepts a forward slash filename separator, but some tools
    // (in particular parts of the Windows/MSVC SDKs) choke on them so we always use the native separator.
    pub fn to_native_relative_path(&self) -> Result<Cow<Path>, ToNativeError> {
        let segments = self.segments();

        #[cfg(unix)]
        {
            use std::{ffi::OsStr, os::unix::prelude::*};

            if !segments.as_label().as_str().contains(':') {
                // The produced path doesn't require any changes, so we can just cast it from the UTF-8

                return Ok(Cow::Borrowed(Path::new(OsStr::from_bytes(
                    segments.as_label().as_str().as_bytes(),
                ))));
            }
        }

        let mut path = PathBuf::with_capacity(self.as_str().len());

        for segment in segments {
            match segment {
                Segment::Filename(name) => path.push(name.as_str()),
                Segment::CurrentDirectory => path.push("."),
                Segment::ParentDirectory => path.push(".."),
                Segment::All => return Err(ToNativeError::ContainsAll),
                Segment::Colon => {}
            }
        }

        Ok(Cow::Owned(path))
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[inline]
    pub fn to_label_buf(&self) -> LabelBuf {
        LabelBuf(self.0.to_owned())
    }

    pub fn join(&self, label: &Label) -> Result<LabelBuf, JoinError> {
        match (self.root(), label.root()) {
            (Root::Workspace(name), Root::WorkspaceRelative) => Ok(LabelBuf(format!("@{}{}", name, label))),
            (_, Root::WorkspaceRelative) => Ok(label.to_label_buf()),
            (_, Root::Workspace(_)) => Ok(label.to_label_buf()),
            (_, Root::PackageRelative) => {
                if self.is_target() && label.is_target() {
                    return Err(JoinError::MultiplePackageSeparators);
                }
                if label.as_str().starts_with(':') || self.as_str().ends_with('/') {
                    Ok(LabelBuf(format!("{}{}", self, label)))
                } else {
                    Ok(LabelBuf(format!("{}/{}", self, label)))
                }
            }
        }
    }

    pub fn join_action(&self, action_id: &str) -> Result<LabelBuf, JoinError> {
        // FIXME: validation, handle edge cases
        Ok(LabelBuf(format!("{}:{}", self, action_id)))
    }

    const fn from_str_unchecked(s: &str) -> &Label {
        unsafe { &*(s as *const str as *const Label) }
    }
}

#[derive(Debug)]
pub struct Parts<'a> {
    pub full: &'a Label,
    pub root: Root<'a>,
    pub package: Option<&'a LabelPath>,
    pub target: Option<&'a LabelPath>,
    pub action_id: Option<&'a str>,
    pub action_path: Option<&'a LabelPath>,
}

impl<'a> Parts<'a> {
    pub fn full_package(&self) -> &'a Label {
        let tail_length = match (self.target, self.action_id, self.action_path) {
            (Some(target_name), Some(action_id), Some(action_path)) => {
                1 + target_name.len() + 1 + action_id.len() + 1 + action_path.len()
            }
            (Some(target_name), Some(action_id), None) => 1 + target_name.len() + 1 + action_id.len(),
            (Some(target_name), None, None) => 1 + target_name.len(),
            (None, None, None) => 0,
            _ => unreachable!(),
        };
        Label::from_str_unchecked(&self.full.as_str()[..(self.full.len() - tail_length)])
    }

    pub fn full_target(&self) -> &'a Label {
        let tail_length = match (self.action_id, self.action_path) {
            (Some(action_id), Some(action_path)) => 1 + action_id.len() + 1 + action_path.len(),
            (Some(action_id), None) => 1 + action_id.len(),
            (None, None) => 0,
            _ => unreachable!(),
        };
        Label::from_str_unchecked(&self.full.as_str()[..(self.full.len() - tail_length)])
    }

    pub fn full_action(&self) -> Option<&'a Label> {
        if !self.action_id.is_some() {
            return None;
        }
        let tail_length = match self.action_path {
            Some(action_path) => 1 + action_path.len(),
            None => 0,
        };
        Some(Label::from_str_unchecked(
            &self.full.as_str()[..(self.full.len() - tail_length)],
        ))
    }
}

impl LabelBuf {
    pub fn new<S: Into<String>>(s: S) -> Result<LabelBuf, ParseError> {
        let s = s.into();
        // Create zero-copy &Label just to check if the label is valid
        let _label = Label::new(&s)?;

        Ok(LabelBuf(s))
    }

    pub fn from_native_relative_path<P: AsRef<Path> + ?Sized>(p: &P) -> Result<LabelBuf, ParseError> {
        Label::from_native_relative_path(p.as_ref()).map(|x| x.into_owned())
    }

    pub fn from_native_relative_pathbuf(p: PathBuf) -> Result<LabelBuf, ParseError> {
        // TODO: zero copy case for forward slash paths
        Label::from_native_relative_path(&p).map(|x| x.into_owned())
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn push<P: AsRef<Label>>(&mut self, label: P) -> Result<(), JoinError> {
        let label = label.as_ref();

        let join_case = match (self.root(), label.root()) {
            (Root::Workspace(name), Root::WorkspaceRelative) => {
                JoinCase::WorkspaceToWorkspaceRelative { name_len: name.len() }
            }
            (_, Root::WorkspaceRelative) => JoinCase::NonWorkspaceToWorkspaceRelative,
            (_, Root::Workspace(_)) => JoinCase::ToWorkspace,
            (_, Root::PackageRelative) => JoinCase::ToPackageRelative,
        };

        match join_case {
            JoinCase::WorkspaceToWorkspaceRelative { name_len } => {
                self.0.truncate(1 + name_len);
                self.0.push_str(label.as_str());
            }
            JoinCase::NonWorkspaceToWorkspaceRelative | JoinCase::ToWorkspace => {
                self.0.clear();
                self.0.push_str(label.as_str());
            }
            JoinCase::ToPackageRelative => {
                if self.is_target() && label.is_target() {
                    return Err(JoinError::MultiplePackageSeparators);
                }
                if label.as_str().starts_with(":") {
                    self.0.push_str(label.as_str())
                } else {
                    if !self.0.ends_with('/') && !self.0.ends_with(':') {
                        self.0.push('/');
                    }
                    self.0.push_str(label.as_str())
                }
            }
        }

        Ok(())
    }

    pub fn pop(&mut self) -> bool {
        // TODO: don't scan from start
        let Some(last_segment) = self.segments().last() else {
            return false;
        };
        match last_segment {
            Segment::CurrentDirectory => todo!(),
            Segment::ParentDirectory => todo!(),
            Segment::All => todo!(),
            Segment::Filename(filename) => {
                let scan_len = if self.0.ends_with('/') {
                    self.0.len() - filename.len() - 1
                } else {
                    self.0.len() - filename.len()
                };
                self.0.replace_range(scan_len.., "");
                true
            }
            Segment::Colon => todo!(),
        }
    }
}

enum JoinCase {
    WorkspaceToWorkspaceRelative { name_len: usize },
    NonWorkspaceToWorkspaceRelative,
    ToWorkspace,
    ToPackageRelative,
}

#[derive(Clone)]
pub struct Segments<'a> {
    root: Root<'a>,
    iter: Chars<'a>,
}

impl<'a> Segments<'a> {
    pub fn root(&self) -> &Root<'a> {
        &self.root
    }

    pub fn as_label(&self) -> &'a Label {
        Label::from_str_unchecked(self.iter.as_str())
    }
}

impl<'a> Iterator for Segments<'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut forward = self.iter.clone();
        let segment = loop {
            match forward.next() {
                Some('/') => {
                    let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len() - 1);
                    // Skip slash
                    self.iter = forward;
                    break segment;
                }
                Some(':') => {
                    if self.iter.as_str().len() - 1 == forward.as_str().len() {
                        self.iter = forward;
                        break Segment::Colon;
                    } else {
                        let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len() - 1);
                        // Don't skip colon, we want to emit it
                        self.iter =
                            self.iter.as_str()[(self.iter.as_str().len() - forward.as_str().len() - 1)..].chars();
                        break segment;
                    }
                }
                None => {
                    if self.iter.as_str().len() == 0 {
                        return None;
                    } else {
                        let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len());
                        self.iter = forward;
                        break segment;
                    }
                }
                Some(c) if valid_filename_char(c) => {}
                Some(_c) => unreachable!(),
            }
        };

        Some(segment)
    }
}

impl<'a> Segments<'a> {
    fn handle_simple_segment(&mut self, consumed_chars: usize) -> Segment<'a> {
        let substr = &self.iter.as_str()[..consumed_chars];
        match substr {
            "." => Segment::CurrentDirectory,
            ".." => Segment::ParentDirectory,
            "..." => Segment::All,
            other => Segment::Filename(Label::from_str_unchecked(other)),
        }
    }
}

#[derive(Clone)]
struct SegmentsChecker<'a> {
    iter: Chars<'a>,
    prev_segment_was_colon: bool,
}

impl<'a> SegmentsChecker<'a> {
    #[inline]
    fn new(cont: &'a str) -> SegmentsChecker<'a> {
        SegmentsChecker {
            iter: cont.chars(),
            prev_segment_was_colon: false,
        }
    }
}

impl<'a> Iterator for SegmentsChecker<'a> {
    type Item = Result<Segment<'a>, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut forward = self.iter.clone();
        let segment = loop {
            match forward.next() {
                Some('/') => {
                    if self.iter.as_str().len() - 1 == forward.as_str().len() {
                        return Some(Err(ParseError::InvalidSlashStart));
                    } else {
                        self.prev_segment_was_colon = false;
                        let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len() - 1);
                        // Skip slash
                        self.iter = forward;
                        break segment;
                    }
                }
                Some(':') => {
                    if self.iter.as_str().len() - 1 == forward.as_str().len() {
                        if self.prev_segment_was_colon {
                            return Some(Err(ParseError::InvalidColonSeparator));
                        }
                        self.prev_segment_was_colon = true;
                        self.iter = forward;
                        break Segment::Colon;
                    } else {
                        self.prev_segment_was_colon = false;
                        let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len() - 1);
                        // Don't skip colon, we want to emit it
                        self.iter =
                            self.iter.as_str()[(self.iter.as_str().len() - forward.as_str().len() - 1)..].chars();
                        break segment;
                    }
                }
                None => {
                    if self.iter.as_str().len() == 0 {
                        return None;
                    } else {
                        self.prev_segment_was_colon = false;
                        let segment = self.handle_simple_segment(self.iter.as_str().len() - forward.as_str().len());
                        self.iter = forward;
                        break segment;
                    }
                }
                Some(c) if valid_filename_char(c) => {}
                Some(c) => {
                    return Some(Err(ParseError::InvalidCharacter(c)));
                }
            }
        };

        Some(Ok(segment))
    }
}

impl<'a> SegmentsChecker<'a> {
    fn handle_simple_segment(&mut self, consumed_chars: usize) -> Segment<'a> {
        let substr = &self.iter.as_str()[..consumed_chars];
        match substr {
            "." => Segment::CurrentDirectory,
            ".." => Segment::ParentDirectory,
            "..." => Segment::All,
            other => Segment::Filename(Label::from_str_unchecked(other)),
        }
    }
}

fn get_root(s: &str) -> Result<(Root, &str), ParseError> {
    let mut iter = s.chars();

    match iter.next() {
        Some('/') => match iter.next() {
            Some('/') => {
                let mut iter2 = iter.clone();
                // Read ahead to ensure we don't start with /// (which is invalid)
                if iter2.next() == Some('/') {
                    return Err(ParseError::InvalidSlashStart);
                }
                Ok((Root::WorkspaceRelative, iter.as_str()))
            }
            _ => Err(ParseError::InvalidSlashStart),
        },
        Some('@') => loop {
            match iter.next() {
                Some('/') => match iter.next() {
                    Some('/') => {
                        return Ok((
                            Root::Workspace(&s[1..(s.len() - iter.as_str().len() - 2)]),
                            iter.as_str(),
                        ));
                    }
                    _ => {
                        return Ok((Root::PackageRelative, s));
                    }
                },
                Some(':') => return Ok((Root::PackageRelative, s)),
                Some(c) if valid_filename_char(c) => {}
                Some(c) => return Err(ParseError::InvalidCharacter(c)),
                None => {
                    return Ok((
                        Root::Workspace(&s[1..(s.len() - iter.as_str().len() - 2)]),
                        iter.as_str(),
                    ))
                }
            }
        },
        Some(':') | Some('.') => Ok((Root::PackageRelative, s)),
        Some(c) if valid_filename_char(c) => Ok((Root::PackageRelative, s)),
        Some(c) => return Err(ParseError::InvalidCharacter(c)),
        None => Ok((Root::PackageRelative, iter.as_str())),
    }
}

#[inline]
fn valid_filename_char(c: char) -> bool {
    match c {
        'A'..='Z' => true,
        'a'..='z' => true,
        ' ' => true,
        '!'..='.' => true,
        '0'..='9' => true,
        ';'..='@' => true,
        '[' => true,
        ']'..='`' => true,
        '{'..='~' => true,
        c if c as u32 >= 128 => true,
        _ => false,
    }
}

pub(crate) const LABEL_SENTINEL: &str = "$cealn_label";

impl Serialize for Label {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(LABEL_SENTINEL, self.as_str())?;
        map.end()
    }
}

impl Serialize for LabelBuf {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <Label as Serialize>::serialize(&*self, serializer)
    }
}

impl<'de> Deserialize<'de> for LabelBuf {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(LabelBufDeVisitor {})
    }
}

struct LabelBufDeVisitor {}

impl<'de> serde::de::Visitor<'de> for LabelBufDeVisitor {
    type Value = LabelBuf;

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
            .ok_or_else(|| A::Error::custom(format!("expected object with key {:?}", LABEL_SENTINEL)))?;
        if key != LABEL_SENTINEL {
            return Err(A::Error::custom(format!(
                "expected object with key {:?}",
                LABEL_SENTINEL
            )));
        }
        let value: String = map.next_value()?;
        // TODO: avoid clone
        let value = LabelBuf::new(value.clone())
            .map_err(|err| A::Error::custom(format!("invalid label {:?}: {}", value, err)))?;
        Ok(value)
    }
}

#[derive(Error, Debug)]
pub enum NormalizeError {
    #[error("label tries to ascend past its root")]
    EscapesRoot,
}

#[derive(Error, Debug)]
pub enum ToNativeError {
    #[error("label must be relative")]
    NotRelative,
    #[error("label contains an `...` segment")]
    ContainsAll,
}

#[derive(Error, Debug)]
pub enum JoinError {
    #[error("joined label would contain multiple package separators (`:`)")]
    MultiplePackageSeparators,
}

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("labels must start with '@', '//', ':', '.', '..', '...' or a filename")]
    InvalidStart,
    #[error("the character {0:?} is not allowed in label filenames")]
    InvalidCharacter(char),
    #[error("label filenames must be valid Unicode")]
    InvalidUnicode,
    #[error("a colon is not valid in this part of a label")]
    UnexpectedColon,
    #[error("labels may not start with '/' unless they start with precisely two slashes '//'")]
    InvalidSlashStart,
    #[error("labels may only contain single slashes, or double slashes at the start of the root workspace")]
    InvalidSlashSeparator,
    #[error("labels may only contain single colons")]
    InvalidColonSeparator,
    #[error("labels may only contain one colon separator")]
    TooManyColonSeparators,
    #[error("labels cannot end on separators")]
    EndedOnSeparator,
    #[error("filenames with all periods are not allowed")]
    FilenameAllPeriods,
    #[error("an @ before a '//' must prefix a workspace name")]
    EmptyWorkspaceName,
    #[error("absolute native paths cannot be mapped to labels")]
    FromNativeRelativePathsOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Root<'a> {
    WorkspaceRelative,
    PackageRelative,
    Workspace(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Segment<'a> {
    /// .
    CurrentDirectory,
    /// ..
    ParentDirectory,
    /// ...
    All,
    Filename(&'a Label),
    Colon,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Event<'a> {
    Segment(Segment<'a>),
    Colon,
}

impl AsRef<str> for Label {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<Label> for Label {
    #[inline]
    fn as_ref(&self) -> &Label {
        self
    }
}

impl AsRef<Label> for LabelBuf {
    #[inline]
    fn as_ref(&self) -> &Label {
        &**self
    }
}

impl Borrow<Label> for LabelBuf {
    #[inline]
    fn borrow(&self) -> &Label {
        &**self
    }
}

impl Deref for LabelBuf {
    type Target = Label;

    #[inline]
    fn deref(&self) -> &Label {
        unsafe { &*(&*self.0 as *const str as *const Label) }
    }
}

impl ToOwned for Label {
    type Owned = LabelBuf;

    #[inline]
    fn to_owned(&self) -> Self::Owned {
        self.to_label_buf()
    }
}

impl<'a> Into<LabelBuf> for &'a Label {
    #[inline]
    fn into(self) -> LabelBuf {
        self.to_owned()
    }
}

impl<'a> Into<LabelBuf> for &'a LabelBuf {
    #[inline]
    fn into(self) -> LabelBuf {
        self.to_owned()
    }
}

impl FromStr for LabelBuf {
    type Err = ParseError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LabelBuf::new(s)
    }
}

impl TryFrom<String> for LabelBuf {
    type Error = ParseError;

    #[inline]
    fn try_from(s: String) -> Result<Self, Self::Error> {
        LabelBuf::new(s)
    }
}

impl From<LabelBuf> for String {
    #[inline]
    fn from(x: LabelBuf) -> Self {
        x.into_string()
    }
}

impl Display for Label {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Debug for Label {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Label({:?})", &self.0)
    }
}

impl Display for LabelBuf {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Debug for LabelBuf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "LabelBuf({:?})", &self.0)
    }
}

impl Display for WorkspaceName {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Debug for WorkspaceName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WorkspaceName({:?})", &self.0)
    }
}

impl Display for WorkspaceNameBuf {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Debug for WorkspaceNameBuf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "WorkspaceNameBuf({:?})", &self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_relative_label_parse() {
        Label::new("//:abc").unwrap();
        Label::new("//:abc/def").unwrap();
        Label::new("//abc").unwrap();
        Label::new("//@abc").unwrap();
        Label::new("//@abc/def").unwrap();
        Label::new("//@abc:def").unwrap();
        Label::new("//@abc:def/ghi").unwrap();
        Label::new("//@abc/def:ghi/jk").unwrap();
        Label::new("//").unwrap();
    }

    #[test]
    fn absolute_label_parse() {
        Label::new("@w//:abc").unwrap();
        Label::new("@w//:abc/def").unwrap();
        Label::new("@workspace//abc").unwrap();
        Label::new("@workspace//@abc").unwrap();
        Label::new("@workspace//@abc/def").unwrap();
        Label::new("@workspace//@abc:def").unwrap();
        Label::new("@workspace//@abc:def/ghi").unwrap();
        Label::new("@workspace//@abc/def:ghi/jk").unwrap();
        Label::new("@workspace//@abc/def/:ghi/jk").unwrap();
    }

    #[test]
    fn package_relative_label_parse() {
        Label::new("abc").unwrap();
        Label::new(":abc").unwrap();
        Label::new(":abc.xyz").unwrap();
        Label::new(":abc/def").unwrap();
        Label::new("@abc").unwrap();
        Label::new("@abc/def").unwrap();
        Label::new("@abc:def").unwrap();
        Label::new("@abc:def/ghi").unwrap();
        Label::new("@abc/def:ghi/jk").unwrap();
    }

    // FIXME: write tests for invalid labels

    macro_rules! assert_normalize_eq {
        ( $orig:expr, $output:expr ) => {
            assert_eq!(
                &*Label::new($orig).unwrap().normalize().unwrap(),
                Label::new($output).unwrap()
            )
        };
    }

    #[test]
    fn normalize() {
        assert_normalize_eq!("//", "//");
        assert_normalize_eq!("//.", "//");
    }

    #[test]
    fn split_package() {
        assert_eq!(
            Label::new("//:abc").unwrap().split_package(),
            (Some(Label::new("//").unwrap()), Some(Label::new("abc").unwrap()))
        );
        assert_eq!(
            Label::new("//:abc/def").unwrap().split_package(),
            (Some(Label::new("//").unwrap()), Some(Label::new("abc/def").unwrap()))
        );
        assert_eq!(
            Label::new("//abc").unwrap().split_package(),
            (Some(Label::new("//abc").unwrap()), None)
        );
        assert_eq!(
            Label::new("//abc/def").unwrap().split_package(),
            (Some(Label::new("//abc/def").unwrap()), None)
        );
        assert_eq!(
            Label::new("//abc/def:ghi").unwrap().split_package(),
            (Some(Label::new("//abc/def").unwrap()), Some(Label::new("ghi").unwrap()))
        );
        assert_eq!(
            Label::new("//abc/def:ghi/jk").unwrap().split_package(),
            (
                Some(Label::new("//abc/def").unwrap()),
                Some(Label::new("ghi/jk").unwrap())
            )
        );
    }

    #[test]
    fn iter_segments_root_file() {
        assert_eq!(
            Label::new("//:somefile.txt").unwrap().segments().collect::<Vec<_>>(),
            vec![Segment::Colon, Segment::Filename(Label::new("somefile.txt").unwrap())]
        )
    }

    #[test]
    fn label_serialize() {
        let label = Label::new("@abc//def:ghi").unwrap();
        let json = serde_json::to_string(label).unwrap();
        assert_eq!(json, r#"{"$cealn_label":"@abc//def:ghi"}"#);
    }

    #[test]
    fn label_buf_serialize() {
        let label = LabelBuf::new("@abc//def:ghi").unwrap();
        let json = serde_json::to_string(&label).unwrap();
        assert_eq!(json, r#"{"$cealn_label":"@abc//def:ghi"}"#);
    }

    #[test]
    fn label_buf_deserialize() {
        let label: LabelBuf = serde_json::from_str(r#"{"$cealn_label":"@abc//def:ghi"}"#).unwrap();
        assert_eq!(&*label, Label::new("@abc//def:ghi").unwrap());
    }

    #[test]
    fn label_buf_from_native_relative_path() {
        assert_eq!(
            &*Label::from_native_relative_path("a/b/c").unwrap(),
            Label::new("a/b/c").unwrap()
        );
        assert_eq!(
            &*Label::from_native_relative_path(".flake8").unwrap(),
            Label::new(".flake8").unwrap()
        );
    }
}
