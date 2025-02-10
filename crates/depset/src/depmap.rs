use std::{
    collections::HashSet,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    sync::Arc,
};

use cealn_data::depmap::{DepmapHash, DepmapKey, DepmapStorable};
use regex::{Regex, RegexSet};
use ring::digest::SHA256;
use tracing::error;

use smallvec::{smallvec, SmallVec};

type DedupHasher = fxhash::FxHasher64;

pub struct DepMap<K: DepmapKey, V: DepmapStorable> {
    internal_node: Option<Arc<InternalNode<K, V>>>,
}

pub struct Builder<K: DepmapKey, V: DepmapStorable> {
    buffer: Vec<u8>,

    transitive_nodes: Vec<Option<Arc<InternalNode<K, V>>>>,
    hasher: ring::digest::Context,
}

struct InternalNode<K: DepmapKey, V: DepmapStorable> {
    buffer: Vec<u8>,
    transitive_nodes: Vec<Option<Arc<InternalNode<K, V>>>>,
    hash: DepmapHash,
    _phantom: PhantomData<(K, V)>,
}

#[derive(Debug)]
pub struct ParseError;

impl<K: DepmapKey, V: DepmapStorable> DepMap<K, V>
where
    K: PartialEq + Eq + Hash + DepmapStorable,
    V: DepmapStorable,
{
    pub type Builder = Builder<K, V>;

    #[inline]
    pub fn new() -> Self {
        DepMap { internal_node: None }
    }

    #[inline]
    pub fn builder() -> Builder<K, V> {
        Builder::<K, V>::new()
    }

    #[inline]
    pub fn iter(&self) -> Iter<K, V> {
        if let Some(node) = &self.internal_node {
            Iter {
                buffer: &node.buffer,
                transitive_nodes: node.transitive_nodes.iter(),
                nested_iterator: None,
                visited_transitive: Default::default(),
            }
        } else {
            Iter {
                buffer: &[],
                transitive_nodes: [].iter(),
                nested_iterator: None,
                visited_transitive: Default::default(),
            }
        }
    }

    #[inline]
    pub fn transitive_iter(&self) -> TransitiveIter<K, V> {
        if let Some(node) = &self.internal_node {
            TransitiveIter {
                transitive_nodes: node.transitive_nodes.iter(),
            }
        } else {
            TransitiveIter {
                transitive_nodes: [].iter(),
            }
        }
    }

    pub fn get<'a, 's>(&'s self, needle: K::Ref<'a>) -> Result<Option<V::Ref<'s>>, ParseError> {
        // FIXME: accelerate this with a hash lookup
        let iter: Iter<'s, K, V> = self.iter();
        for entry in iter {
            let (k, v) = entry?;
            if <K as DepmapKey>::eq(needle, K::deref_cow(&k)) {
                return Ok(Some(v));
            }
        }

        Ok(None)
    }

    #[inline]
    pub fn hash(&self) -> &DepmapHash {
        match &self.internal_node {
            Some(node) => &node.hash,
            None => &*EMPTY_DEPMAP_HASH,
        }
    }

    #[inline]
    pub fn serialized_bytes(&self) -> &[u8] {
        self.internal_node.as_ref().map(|x| &*x.buffer).unwrap_or(&[])
    }

    pub fn deserialize(buffer: Vec<u8>, transitive_nodes: Vec<Self>, hash: DepmapHash) -> Self {
        DepMap {
            internal_node: Some(Arc::new(InternalNode {
                buffer,
                transitive_nodes: transitive_nodes.into_iter().map(|x| x.internal_node).collect(),
                hash,
                _phantom: PhantomData,
            })),
        }
    }
}

lazy_static::lazy_static! {
    static ref EMPTY_DEPMAP_HASH: DepmapHash = {
        let direct_digest = ring::digest::Context::new(&SHA256).finish();
        let transitive_digest = ring::digest::Context::new(&SHA256).finish();

        let mut aggregate_hasher = ring::digest::Context::new(&SHA256);
        aggregate_hasher.update(direct_digest.as_ref());
        aggregate_hasher.update(transitive_digest.as_ref());
        let aggregate_digest = aggregate_hasher.finish();
        DepmapHash::Sha256(aggregate_digest.as_ref().try_into().unwrap())
    };
}

const ELEMENT_TAG: u8 = 1;
const TRANSITIVE_TAG: u8 = 2;
const FILTER_TAG: u8 = 3;

impl<K: DepmapKey, V: DepmapStorable> Builder<K, V>
where
    K: PartialEq + Eq + Hash + DepmapStorable,
    V: DepmapStorable,
{
    #[inline]
    pub fn new() -> Self {
        Builder {
            buffer: Vec::new(),

            transitive_nodes: Default::default(),
            hasher: ring::digest::Context::new(&SHA256),
        }
    }

    #[inline]
    pub fn insert<'a, 's>(&'s mut self, k: K::Ref<'a>, v: V::Ref<'a>) -> &'s mut Self {
        let init_len = self.buffer.len();
        K::write_bytes(k, &mut self.buffer);
        self.buffer.push(ELEMENT_TAG);
        V::write_bytes(v, &mut self.buffer);
        self.hasher.update(&self.buffer[init_len..]);
        self
    }

    pub fn merge<'a, 's>(&'s mut self, mount: K::Ref<'a>, depmap: DepMap<K, V>) -> &'s mut Self {
        if let Some(node) = &depmap.internal_node {
            let init_len = self.buffer.len();
            K::write_bytes(mount, &mut self.buffer);
            self.buffer.push(TRANSITIVE_TAG);
            match depmap.hash() {
                DepmapHash::Sha256(digest) => self.buffer.extend_from_slice(digest),
            }
            self.transitive_nodes.push(Some(node.clone()));
            self.hasher.update(&self.buffer[init_len..]);
        }
        self
    }

    pub fn merge_filtered<'a, 's, S>(
        &'s mut self,
        mount: K::Ref<'a>,
        prefix: K::Ref<'a>,
        patterns: &[S],
        depmap: DepMap<K, V>,
    ) -> &'s mut Self
    where
        S: AsRef<str>,
    {
        if let Some(node) = &depmap.internal_node {
            let init_len = self.buffer.len();
            K::write_bytes(mount, &mut self.buffer);
            self.buffer.push(FILTER_TAG);
            match depmap.hash() {
                DepmapHash::Sha256(digest) => self.buffer.extend_from_slice(digest),
            }
            self.transitive_nodes.push(Some(node.clone()));
            K::write_bytes(prefix, &mut self.buffer);
            self.buffer.extend_from_slice(&(patterns.len() as u64).to_le_bytes());
            for pattern in patterns {
                let pattern = pattern.as_ref();
                self.buffer.extend_from_slice(&(pattern.len() as u64).to_le_bytes());
                self.buffer.extend_from_slice(pattern.as_bytes());
            }
            self.hasher.update(&self.buffer[init_len..]);
        }
        self
    }

    #[inline]
    pub fn build(&mut self) -> DepMap<K, V> {
        DepMap {
            internal_node: Some(Arc::new(InternalNode {
                buffer: mem::replace(&mut self.buffer, Vec::new()),
                transitive_nodes: mem::replace(&mut self.transitive_nodes, Vec::new()),
                hash: DepmapHash::Sha256(self.hasher.clone().finish().as_ref().try_into().unwrap()),
                _phantom: PhantomData,
            })),
        }
    }
}

pub fn scan_transitive_nodes<K: DepmapKey, V: DepmapStorable>(
    mut buffer: &[u8],
) -> Result<Vec<DepmapHash>, ParseError> {
    let mut nodes = Vec::new();
    while !buffer.is_empty() {
        let (_, tail) = K::from_bytes(buffer).ok_or_else(|| ParseError)?;
        let (tag, tail) = tail.split_first().ok_or_else(|| ParseError)?;
        match *tag {
            ELEMENT_TAG => {
                let (_, tail) = V::from_bytes(tail).ok_or_else(|| ParseError)?;
                buffer = tail;
            }
            TRANSITIVE_TAG => {
                if tail.len() < 32 {
                    return Err(ParseError);
                }
                let (digest, tail) = tail.split_array_ref::<32>();
                let digest = DepmapHash::Sha256(*digest);
                nodes.push(digest);
                buffer = tail;
            }
            FILTER_TAG => {
                if tail.len() < 32 {
                    return Err(ParseError);
                }
                let (digest, tail) = tail.split_array_ref::<32>();
                let digest = DepmapHash::Sha256(*digest);
                nodes.push(digest);
                let Some((_prefix, tail)) = K::from_bytes(tail) else {
                    return Err(ParseError);
                };
                const PATTERN_COUNT_SIZE: usize = mem::size_of::<u64>();
                if tail.len() < PATTERN_COUNT_SIZE {
                    return Err(ParseError);
                }
                let (pattern_count, tail) = tail.split_array_ref::<PATTERN_COUNT_SIZE>();
                let Ok(pattern_count) = usize::try_from(u64::from_le_bytes(*pattern_count)) else {
                    return Err(ParseError);
                };
                let mut pattern_tail = tail;
                for _ in 0..pattern_count {
                    let tail = pattern_tail;
                    const PATTERN_LEN_SIZE: usize = mem::size_of::<u64>();
                    if tail.len() < PATTERN_LEN_SIZE {
                        return Err(ParseError);
                    }
                    let (pattern_len, tail) = tail.split_array_ref::<PATTERN_LEN_SIZE>();
                    let Ok(pattern_len) = usize::try_from(u64::from_le_bytes(*pattern_len)) else {
                        return Err(ParseError);
                    };
                    if tail.len() < pattern_len {
                        return Err(ParseError);
                    }
                    let (_pattern, tail) = tail.split_at(pattern_len);
                    pattern_tail = tail;
                }
                let tail = pattern_tail;
                buffer = tail;
            }
            _ => return Err(ParseError),
        }
    }
    Ok(nodes)
}

pub struct Iter<'a, K: DepmapKey, V: DepmapStorable> {
    buffer: &'a [u8],
    transitive_nodes: std::slice::Iter<'a, Option<Arc<InternalNode<K, V>>>>,
    nested_iterator: Option<NestedIterator<'a, K, V>>,
    visited_transitive: HashSet<DepmapHash>,
}

struct NestedIterator<'a, K: DepmapKey, V: DepmapStorable> {
    mount: K::Ref<'a>,
    prefix: Option<K::Ref<'a>>,
    patterns: Option<RegexSet>,
    iter: Box<Iter<'a, K, V>>,
}

impl<'a, K: DepmapKey, V: DepmapStorable> Iterator for Iter<'a, K, V> {
    type Item = Result<(K::Cow<'a>, V::Ref<'a>), ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(nested_iter) = self.nested_iterator.as_mut() {
                match nested_iter.iter.next() {
                    Some(Ok((sub_k, v))) => {
                        let sub_k = if let Some(prefix) = nested_iter.prefix {
                            let Some(sub_k) = K::strip_prefix(K::deref_cow(&sub_k), prefix) else {
                                continue;
                            };
                            sub_k
                        } else {
                            K::deref_cow(&sub_k)
                        };
                        if let Some(patterns) = &nested_iter.patterns {
                            if !K::is_match(patterns, sub_k) {
                                continue;
                            }
                        }
                        let full_key = K::join(nested_iter.mount, sub_k);
                        return Some(Ok((K::cow_owned(full_key), v)));
                    }
                    Some(Err(err)) => return Some(Err(err)),
                    None => {
                        if nested_iter.patterns.is_none() && nested_iter.prefix.is_none() {
                            self.visited_transitive =
                                mem::replace(&mut nested_iter.iter.visited_transitive, HashSet::new());
                        }
                        self.nested_iterator = None;
                    }
                }
            }

            if self.buffer.is_empty() {
                return None;
            }
            let Some((k, tail)) = K::from_bytes(self.buffer) else {
            return Some(Err(ParseError));
        };
            let Some((tag, tail)) = tail.split_first() else {
            return Some(Err(ParseError));
        };
            match *tag {
                ELEMENT_TAG => {
                    let Some((v, tail)) = V::from_bytes(tail) else {
                    return Some(Err(ParseError));
                };
                    self.buffer = tail;
                    return Some(Ok((K::cow_borrowed(k), v)));
                }
                TRANSITIVE_TAG => {
                    if tail.len() < 32 {
                        return Some(Err(ParseError));
                    }
                    let (digest, tail) = tail.split_array_ref();
                    let Some(node) = self.transitive_nodes.next() else {
                        return Some(Err(ParseError));
                    };
                    let node_hash = DepmapHash::Sha256(*digest);
                    if !self.visited_transitive.contains(&node_hash) {
                        self.visited_transitive.insert(node_hash.clone());
                        if let Some(node) = node {
                            if node.hash != node_hash {
                                return Some(Err(ParseError));
                            }
                            self.nested_iterator = Some(NestedIterator {
                                mount: k,
                                prefix: None,
                                patterns: None,
                                iter: Box::new(Iter {
                                    buffer: &node.buffer,
                                    transitive_nodes: node.transitive_nodes.iter(),
                                    nested_iterator: None,
                                    visited_transitive: mem::replace(&mut self.visited_transitive, HashSet::new()),
                                }),
                            });
                        }
                    }
                    self.buffer = tail;
                    continue;
                }
                FILTER_TAG => {
                    if tail.len() < 32 {
                        error!("no space for transitive depmap hash");
                        return Some(Err(ParseError));
                    }
                    let (digest, tail) = tail.split_array_ref();
                    let Some(node) = self.transitive_nodes.next() else {
                        error!("more transitive depmap referneces in data that we have nodes");
                        return Some(Err(ParseError));
                    };
                    let Some((prefix, tail)) = K::from_bytes(tail) else {
                        error!("failed to parse prefix");
                        return Some(Err(ParseError));
                    };
                    const PATTERN_COUNT_SIZE: usize = mem::size_of::<u64>();
                    if tail.len() < PATTERN_COUNT_SIZE {
                        error!("no space for pattern count");
                        return Some(Err(ParseError));
                    }
                    let (pattern_count, tail) = tail.split_array_ref::<PATTERN_COUNT_SIZE>();
                    let Ok(pattern_count) = usize::try_from(u64::from_le_bytes(*pattern_count)) else {
                        error!("pattern count too large");
                        return Some(Err(ParseError));
                    };
                    let mut patterns = Vec::with_capacity(pattern_count);
                    let mut pattern_tail = tail;
                    for _ in 0..pattern_count {
                        let tail = pattern_tail;
                        const PATTERN_LEN_SIZE: usize = mem::size_of::<u64>();
                        if tail.len() < PATTERN_LEN_SIZE {
                            error!("no space for pattern length");
                            return Some(Err(ParseError));
                        }
                        let (pattern_len, tail) = tail.split_array_ref::<PATTERN_LEN_SIZE>();
                        let Ok(pattern_len) = usize::try_from(u64::from_le_bytes(*pattern_len)) else {
                            error!("pattern length too big");
                            return Some(Err(ParseError));
                        };
                        if tail.len() < pattern_len {
                            error!("not enough data for pattern");
                            return Some(Err(ParseError));
                        }
                        let (pattern, tail) = tail.split_at(pattern_len);
                        let Ok(pattern) = std::str::from_utf8(pattern) else {
                            error!("invalid UTF-8 in regex pattern");
                            return Some(Err(ParseError));
                        };
                        patterns.push(pattern);
                        pattern_tail = tail;
                    }
                    let tail = pattern_tail;
                    let Ok(patterns) = RegexSet::new(&patterns) else {
                        error!(?patterns, "invalid regex pattern");
                        return Some(Err(ParseError));
                    };
                    if let Some(node) = node {
                        if node.hash != DepmapHash::Sha256(*digest) {
                            error!("depmap hash mismatch");
                            return Some(Err(ParseError));
                        }
                        self.nested_iterator = Some(NestedIterator {
                            mount: k,
                            prefix: Some(prefix),
                            patterns: Some(patterns),
                            iter: Box::new(Iter {
                                buffer: &node.buffer,
                                transitive_nodes: node.transitive_nodes.iter(),
                                nested_iterator: None,
                                visited_transitive: HashSet::new(),
                            }),
                        });
                    }
                    self.buffer = tail;
                    continue;
                }
                _ => return Some(Err(ParseError)),
            }
        }
    }
}

pub struct TransitiveIter<'a, K: DepmapKey, V: DepmapStorable> {
    transitive_nodes: std::slice::Iter<'a, Option<Arc<InternalNode<K, V>>>>,
}

impl<'a, K: DepmapKey, V: DepmapStorable> Iterator for TransitiveIter<'a, K, V> {
    type Item = DepMap<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.transitive_nodes.next().map(|x| DepMap {
            internal_node: x.clone(),
        })
    }
}

impl<K: DepmapKey, V: DepmapStorable> Clone for DepMap<K, V> {
    #[inline]
    fn clone(&self) -> Self {
        DepMap {
            internal_node: self.internal_node.clone(),
        }
    }
}

impl<K: DepmapKey, V: DepmapStorable> Default for DepMap<K, V> {
    #[inline]
    fn default() -> Self {
        DepMap { internal_node: None }
    }
}

impl std::error::Error for ParseError {}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("corrupted depmap")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_dedup() {
        let set = DepMap::builder()
            .insert("hello", "world")
            .insert("hello", "you")
            .build();

        let internal_node = set.internal_node.unwrap();
        assert_eq!(internal_node.direct.len(), 1);
        assert_eq!(internal_node.direct[0], ("hello", "you"));
    }
}
