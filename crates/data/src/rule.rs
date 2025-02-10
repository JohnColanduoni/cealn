use std::{borrow::Cow, collections::BTreeMap, fmt, ops::Deref};

use serde::{
    de::{Error as DeError, Visitor},
    ser::SerializeMap,
    Deserialize, Serialize,
};

use crate::{action::LabelAction, reference::Reference, Label};

/// An implementation of a particular build process that can be used to generate [`Target`]s
#[derive(Clone, Debug)]
pub struct Rule {
    reference: Reference,
}

/// A concrete set of parameters for invoking a [`Rule`]
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Target {
    pub name: String,
    pub rule: Reference,
    pub attributes_input: BTreeMap<String, serde_json::Value>,
    pub output_mounts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Analysis {
    pub actions: Vec<LabelAction>,
    pub synthetic_targets: Vec<Target>,
    #[serde(default)]
    pub providers: Vec<Provider>,
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub reference: Reference,
    pub data: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BuildConfig {
    pub options: Vec<(Reference, Reference)>,
    pub host_options: Vec<(Reference, Reference)>,
}

impl BuildConfig {
    pub fn transition_to_host(&self) -> BuildConfig {
        BuildConfig {
            options: self.host_options.clone(),
            host_options: self.host_options.clone(),
        }
    }
}

impl fmt::Debug for BuildConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BuildConfig {{")?;
        write!(f, " options: [")?;
        let mut first = true;
        for (k, v) in &self.options {
            if first {
                first = false;
            } else {
                write!(f, ", ")?;
            }
            write!(f, "{}={}", k.qualname, v.qualname)?;
        }
        write!(f, "],")?;
        write!(f, " host_options: [")?;
        let mut first = true;
        for (k, v) in &self.host_options {
            if first {
                first = false;
            } else {
                write!(f, ", ")?;
            }
            write!(f, "{}={}", k.qualname, v.qualname)?;
        }
        write!(f, "] }}")?;
        Ok(())
    }
}

pub(crate) const PROVIDER_SENTINEL: &str = "$cealn_provider";

impl Serialize for Provider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            PROVIDER_SENTINEL,
            &ProviderRepr {
                source_label: Cow::Borrowed(&self.reference.source_label),
                qualname: Cow::Borrowed(&self.reference.qualname),
                data: Cow::Borrowed(&self.data),
            },
        )?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProviderDeVisitor)
    }
}

struct ProviderDeVisitor;

#[derive(Serialize, Deserialize)]
pub(crate) struct ProviderRepr<'a, D = BTreeMap<String, serde_json::Value>>
where
    D: Clone,
{
    pub(crate) source_label: Cow<'a, Label>,
    pub(crate) qualname: Cow<'a, str>,
    pub(crate) data: Cow<'a, D>,
}

impl<'de> Visitor<'de> for ProviderDeVisitor {
    type Value = Provider;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "Provider")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let key: Cow<str> = map
            .next_key()?
            .ok_or_else(|| A::Error::custom(format!("expected object with key {:?}", PROVIDER_SENTINEL)))?;
        if key != PROVIDER_SENTINEL {
            return Err(A::Error::custom(format!(
                "expected object with key {:?}",
                PROVIDER_SENTINEL
            )));
        }
        let value: ProviderRepr<'static> = map.next_value()?;
        Ok(Provider {
            reference: Reference {
                source_label: value.source_label.into_owned(),
                qualname: value.qualname.into_owned(),
            },
            data: value.data.into_owned(),
        })
    }
}

impl Deref for Rule {
    type Target = Reference;

    fn deref(&self) -> &Reference {
        &self.reference
    }
}
