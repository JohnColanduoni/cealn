pub mod fs;
pub mod package_load;
pub mod rule;
pub mod workspace_load;

use std::{borrow::Cow, fmt, marker::PhantomData};

use serde::{
    de::{
        self,
        value::{CowStrDeserializer, MapAccessDeserializer, MapDeserializer},
    },
    Deserialize, Deserializer, Serialize,
};
use thiserror::Error;

#[derive(Debug)]
pub enum InvocationResult<T> {
    Ok(T),
    Err(Error),
}

#[derive(Serialize, Deserialize, Error, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("python error: {0}")]
    Python(PythonError),
}
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct PythonError {
    pub class: String,
    pub message: String,
    pub traceback: Option<String>,
}

#[derive(Debug)]
#[repr(u32)]
pub enum DataEncoding {
    Utf8 = 1,
    Latin1 = 2,
}

impl<T> InvocationResult<T> {
    pub fn into_result(self) -> Result<T, Error> {
        match self {
            InvocationResult::Ok(data) => Ok(data),
            InvocationResult::Err(err) => Err(err),
        }
    }
}

impl fmt::Display for PythonError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.message)?;
        if let Some(traceback) = &self.traceback {
            write!(f, "\n{}", traceback)?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct InvocationResultErrRepr<'a> {
    error: &'a Error,
}

impl<T> Serialize for InvocationResult<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            InvocationResult::Ok(ok) => ok.serialize(serializer),
            InvocationResult::Err(err) => InvocationResultErrRepr { error: err }.serialize(serializer),
        }
    }
}

impl<'de, T> Deserialize<'de> for InvocationResult<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(InvocationResultVisitor { _phantom: PhantomData })
    }
}

struct InvocationResultVisitor<T> {
    _phantom: PhantomData<T>,
}

impl<'de, T> de::Visitor<'de> for InvocationResultVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = InvocationResult<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "invocation result")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        match map.next_key::<Cow<'de, str>>()? {
            Some(ref key) if &*key == "error" => {
                let error: Error = map.next_value()?;
                Ok(InvocationResult::Err(error))
            }
            Some(key) => {
                let value = T::deserialize(MapAccessDeserializer::new(PutBackMapAccess {
                    key: Some(key),
                    inner: map,
                }))?;
                Ok(InvocationResult::Ok(value))
            }
            None => {
                let value = T::deserialize(MapDeserializer::<_, A::Error>::new(std::iter::empty::<((), ())>()))?;
                Ok(InvocationResult::Ok(value))
            }
        }
    }
}

struct PutBackMapAccess<'de, A> {
    key: Option<Cow<'de, str>>,
    inner: A,
}

impl<'de, A> de::MapAccess<'de> for PutBackMapAccess<'de, A>
where
    A: de::MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: de::DeserializeSeed<'de>,
    {
        if let Some(key) = self.key.take() {
            K::deserialize(seed, CowStrDeserializer::new(key)).map(Some)
        } else {
            self.inner.next_key_seed(seed)
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: de::DeserializeSeed<'de>,
    {
        self.inner.next_value_seed(seed)
    }
}
