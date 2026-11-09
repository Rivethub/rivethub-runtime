use std::borrow::Cow;

use serde::Serialize;
use serde::ser;
use serde::ser::Error as _;
use serde::ser::Impossible;
use serde_json::Error;

const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    value.serialize(Canonical { out: &mut out })?;
    Ok(out)
}

struct Canonical<'a> {
    out: &'a mut Vec<u8>,
}

impl Canonical<'_> {
    fn scalar<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        serde_json::to_writer(&mut *self.out, value)
    }

    fn open_variant(&mut self, variant: &str) -> Result<(), Error> {
        self.out.push(b'{');
        serde_json::to_writer(&mut *self.out, variant)?;
        self.out.push(b':');
        Ok(())
    }
}

impl<'a> ser::Serializer for Canonical<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Items<'a>;
    type SerializeTuple = Items<'a>;
    type SerializeTupleStruct = Items<'a>;
    type SerializeTupleVariant = Items<'a>;
    type SerializeMap = Members<'a>;
    type SerializeStruct = Members<'a>;
    type SerializeStructVariant = Members<'a>;

    fn serialize_bool(self, value: bool) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_i8(self, value: i8) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_i16(self, value: i16) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_i32(self, value: i32) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_i64(self, value: i64) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_i128(self, value: i128) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_u8(self, value: u8) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_u16(self, value: u16) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_u32(self, value: u32) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_u64(self, value: u64) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_u128(self, value: u128) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_f32(self, value: f32) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_f64(self, value: f64) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_char(self, value: char) -> Result<(), Error> {
        self.scalar(&value)
    }
    fn serialize_str(self, value: &str) -> Result<(), Error> {
        self.scalar(value)
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<(), Error> {
        self.scalar(value)
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.scalar(&())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.scalar(&())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.scalar(&())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.scalar(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        if name == RAW_VALUE_TOKEN {
            return Err(Error::custom(
                "canonical JSON cannot reorder a raw JSON value",
            ));
        }
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        mut self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.open_variant(variant)?;
        value.serialize(Canonical {
            out: &mut *self.out,
        })?;
        self.out.push(b'}');
        Ok(())
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Items<'a>, Error> {
        Ok(Items::open(self.out, false))
    }
    fn serialize_tuple(self, _len: usize) -> Result<Items<'a>, Error> {
        Ok(Items::open(self.out, false))
    }
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Items<'a>, Error> {
        Ok(Items::open(self.out, false))
    }
    fn serialize_tuple_variant(
        mut self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Items<'a>, Error> {
        self.open_variant(variant)?;
        Ok(Items::open(self.out, true))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Members<'a>, Error> {
        Ok(Members::open(self.out, false))
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Members<'a>, Error> {
        Ok(Members::open(self.out, false))
    }
    fn serialize_struct_variant(
        mut self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Members<'a>, Error> {
        self.open_variant(variant)?;
        Ok(Members::open(self.out, true))
    }
}

struct Items<'a> {
    out: &'a mut Vec<u8>,
    empty: bool,
    in_variant: bool,
}

impl<'a> Items<'a> {
    fn open(out: &'a mut Vec<u8>, in_variant: bool) -> Self {
        out.push(b'[');
        Self {
            out,
            empty: true,
            in_variant,
        }
    }

    fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        if !self.empty {
            self.out.push(b',');
        }
        self.empty = false;
        value.serialize(Canonical {
            out: &mut *self.out,
        })
    }

    fn close(self) {
        self.out.push(b']');
        if self.in_variant {
            self.out.push(b'}');
        }
    }
}

impl ser::SerializeSeq for Items<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.item(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close();
        Ok(())
    }
}

impl ser::SerializeTuple for Items<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.item(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close();
        Ok(())
    }
}

impl ser::SerializeTupleStruct for Items<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.item(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close();
        Ok(())
    }
}

impl ser::SerializeTupleVariant for Items<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.item(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close();
        Ok(())
    }
}

struct Member {
    key: Cow<'static, str>,
    start: usize,
    end: usize,
}

// Entries are written in arrival order, each recorded as the byte range of `"key":value`; `close` rewrites the range
// in key order only when they did not already arrive sorted.
struct Members<'a> {
    out: &'a mut Vec<u8>,
    body: usize,
    entries: Vec<Member>,
    pending: Option<(Cow<'static, str>, usize)>,
    in_variant: bool,
}

impl<'a> Members<'a> {
    fn open(out: &'a mut Vec<u8>, in_variant: bool) -> Self {
        out.push(b'{');
        let body = out.len();
        Self {
            out,
            body,
            entries: Vec::new(),
            pending: None,
            in_variant,
        }
    }

    fn key(&mut self, key: Cow<'static, str>) -> Result<(), Error> {
        if !self.entries.is_empty() {
            self.out.push(b',');
        }
        let start = self.out.len();
        serde_json::to_writer(&mut *self.out, key.as_ref())?;
        self.out.push(b':');
        self.pending = Some((key, start));
        Ok(())
    }

    fn value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let Some((key, start)) = self.pending.take() else {
            return Err(Error::custom("a map value arrived before its key"));
        };
        value.serialize(Canonical {
            out: &mut *self.out,
        })?;
        self.entries.push(Member {
            key,
            start,
            end: self.out.len(),
        });
        Ok(())
    }

    fn close(mut self) -> Result<(), Error> {
        let sorted = self
            .entries
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left.key <= right.key));
        if !sorted {
            let written = self.out.split_off(self.body);
            self.entries.sort_by(|left, right| left.key.cmp(&right.key));
            for (index, member) in self.entries.iter().enumerate() {
                if index > 0 {
                    self.out.push(b',');
                }
                let range = member.start - self.body..member.end - self.body;
                let bytes = written
                    .get(range)
                    .ok_or_else(|| Error::custom("a member's bytes were lost while sorting"))?;
                self.out.extend_from_slice(bytes);
            }
        }
        self.out.push(b'}');
        if self.in_variant {
            self.out.push(b'}');
        }
        Ok(())
    }
}

impl ser::SerializeMap for Members<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        let key = key.serialize(KeyText)?;
        self.key(Cow::Owned(key))
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.value(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close()
    }
}

impl ser::SerializeStruct for Members<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.key(Cow::Borrowed(key))?;
        self.value(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close()
    }
}

impl ser::SerializeStructVariant for Members<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.key(Cow::Borrowed(key))?;
        self.value(value)
    }
    fn end(self) -> Result<(), Error> {
        self.close()
    }
}

struct KeyText;

fn not_a_key() -> Error {
    Error::custom("a canonical JSON object key must be a string")
}

impl ser::Serializer for KeyText {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = Impossible<String, Error>;
    type SerializeTuple = Impossible<String, Error>;
    type SerializeTupleStruct = Impossible<String, Error>;
    type SerializeTupleVariant = Impossible<String, Error>;
    type SerializeMap = Impossible<String, Error>;
    type SerializeStruct = Impossible<String, Error>;
    type SerializeStructVariant = Impossible<String, Error>;

    fn serialize_str(self, value: &str) -> Result<String, Error> {
        Ok(value.to_owned())
    }
    fn serialize_char(self, value: char) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_bool(self, value: bool) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_i8(self, value: i8) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_i16(self, value: i16) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_i32(self, value: i32) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_i64(self, value: i64) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_u8(self, value: u8) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_u16(self, value: u16) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_u32(self, value: u32) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_u64(self, value: u64) -> Result<String, Error> {
        Ok(value.to_string())
    }
    fn serialize_f32(self, _value: f32) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_f64(self, _value: f64) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_bytes(self, _value: &[u8]) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_none(self) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_unit(self) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String, Error> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String, Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String, Error> {
        Err(not_a_key())
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(not_a_key())
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
        Err(not_a_key())
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(not_a_key())
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(not_a_key())
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(not_a_key())
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        Err(not_a_key())
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(not_a_key())
    }
}
