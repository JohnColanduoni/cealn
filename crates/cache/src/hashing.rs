use serde::{
    ser::{
        SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple, SerializeTupleStruct,
        SerializeTupleVariant,
    },
    Serialize, Serializer,
};
use thiserror::Error;

pub fn hash_serializable<T>(value: &T) -> ring::digest::Digest
where
    T: Serialize,
{
    let mut serializer = HashingSerializer {
        hasher: ring::digest::Context::new(&ring::digest::SHA256),
    };
    value.serialize(&mut serializer).unwrap();
    serializer.hasher.finish()
}

struct HashingSerializer {
    hasher: ring::digest::Context,
}

#[repr(u8)]
enum Typecode {
    String = 0,
    Map,
    Seq,
    Bool,
    None,
    Struct,
    NewTypeVariant,
    Tuple,
    U8,
    Some,
    UnitVariant,
    StructVariant,
    U32,
    Unit,
    U64,
}

const ITEM_CODE: u8 = 1;
const END_CODE: u8 = 2;

#[derive(Error, Debug)]
enum HashingSerializerError {
    #[error("{0}")]
    Custom(String),
}

impl<'a> Serializer for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    type SerializeSeq = Self;

    type SerializeTuple = Self;

    type SerializeTupleStruct = Self;

    type SerializeTupleVariant = Self;

    type SerializeMap = Self;

    type SerializeStruct = Self;

    type SerializeStructVariant = Self;

    #[inline]
    fn is_human_readable(&self) -> bool {
        false
    }

    #[inline]
    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::Bool as u8, if v { 1 } else { 0 }]);
        Ok(())
    }

    #[inline]
    fn serialize_i8(self, _v: i8) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_i16(self, _v: i16) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_i32(self, _v: i32) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_i64(self, _v: i64) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::U8 as u8]);
        self.hasher.update(&[v]);
        Ok(())
    }

    #[inline]
    fn serialize_u16(self, _v: u16) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::U32 as u8]);
        self.hasher.update(&v.to_le_bytes());
        Ok(())
    }

    #[inline]
    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::U64 as u8]);
        self.hasher.update(&v.to_le_bytes());
        Ok(())
    }

    #[inline]
    fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_char(self, _v: char) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::String as u8]);
        self.hasher.update(&(v.len() as u64).to_le_bytes());
        self.hasher.update(v.as_bytes());
        Ok(())
    }

    #[inline]
    fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::None as u8]);
        Ok(())
    }

    #[inline]
    fn serialize_some<T: ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[Typecode::Some as u8]);
        value.serialize(self)
    }

    #[inline]
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::Unit as u8]);
        Ok(())
    }

    #[inline]
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_unit_variant(
        self,
        name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[Typecode::UnitVariant as u8]);
        self.hasher.update(&(name.len() as u64).to_le_bytes());
        self.hasher.update(name.as_bytes());
        self.hasher.update(&(variant.len() as u64).to_le_bytes());
        self.hasher.update(variant.as_bytes());
        Ok(())
    }

    #[inline]
    fn serialize_newtype_struct<T: ?Sized>(self, _name: &'static str, _value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        todo!()
    }

    #[inline]
    fn serialize_newtype_variant<T: ?Sized>(
        self,
        name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[Typecode::NewTypeVariant as u8]);
        self.hasher.update(&(name.len() as u64).to_le_bytes());
        self.hasher.update(name.as_bytes());
        self.hasher.update(&(variant.len() as u64).to_le_bytes());
        self.hasher.update(variant.as_bytes());
        value.serialize(self)
    }

    #[inline]
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.hasher.update(&[Typecode::Seq as u8]);
        Ok(self)
    }

    #[inline]
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.hasher.update(&[Typecode::Tuple as u8]);
        Ok(self)
    }

    #[inline]
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        todo!()
    }

    #[inline]
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        self.hasher.update(&[Typecode::Map as u8]);
        Ok(self)
    }

    #[inline]
    fn serialize_struct(self, name: &'static str, _len: usize) -> Result<Self::SerializeStruct, Self::Error> {
        self.hasher.update(&[Typecode::Struct as u8]);
        self.hasher.update(&(name.len() as u64).to_le_bytes());
        self.hasher.update(name.as_bytes());
        Ok(self)
    }

    #[inline]
    fn serialize_struct_variant(
        self,
        name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.hasher.update(&[Typecode::StructVariant as u8]);
        self.hasher.update(&(name.len() as u64).to_le_bytes());
        self.hasher.update(name.as_bytes());
        self.hasher.update(&(variant.len() as u64).to_le_bytes());
        self.hasher.update(variant.as_bytes());
        Ok(self)
    }
}

impl<'a> SerializeSeq for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_element<T: ?Sized>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[ITEM_CODE]);
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[END_CODE]);
        Ok(())
    }
}

impl<'a> SerializeTuple for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_element<T: ?Sized>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[ITEM_CODE]);
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[END_CODE]);
        Ok(())
    }
}

impl<'a> SerializeTupleStruct for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_field<T: ?Sized>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        todo!()
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        todo!()
    }
}

impl<'a> SerializeTupleVariant for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_field<T: ?Sized>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        todo!()
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        todo!()
    }
}

impl<'a> SerializeMap for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_key<T: ?Sized>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[ITEM_CODE]);
        key.serialize(&mut **self)
    }

    fn serialize_value<T: ?Sized>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[END_CODE]);
        Ok(())
    }
}

impl<'a> SerializeStruct for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_field<T: ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[ITEM_CODE]);
        self.hasher.update(&(key.len() as u64).to_le_bytes());
        self.hasher.update(key.as_bytes());
        value.serialize(&mut **self)?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[END_CODE]);
        Ok(())
    }
}

impl<'a> SerializeStructVariant for &'a mut HashingSerializer {
    type Ok = ();

    type Error = HashingSerializerError;

    fn serialize_field<T: ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        self.hasher.update(&[ITEM_CODE]);
        self.hasher.update(&(key.len() as u64).to_le_bytes());
        self.hasher.update(key.as_bytes());
        value.serialize(&mut **self)?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.hasher.update(&[END_CODE]);
        Ok(())
    }
}

impl serde::ser::Error for HashingSerializerError {
    fn custom<T>(msg: T) -> Self
    where
        T: std::fmt::Display,
    {
        HashingSerializerError::Custom(msg.to_string())
    }
}
