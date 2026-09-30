// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Protocol details from arthursoares/roon-api-reverse-engineering (MIT), see NOTICE.

//! The object graph: everything the Core pushes to us over the session.
//!
//! The Core first declares its types (DEFTYPE: id, name, ordered members with
//! a PropertyType each), then pushes objects (PUSHOBJ / UPDATEOBJ: object id,
//! type id, fields). Knowing each member's PropertyType lets us read any
//! object generically.
//!
//! Fields are sparse: `flexInt(member index, 1-based) value`, repeated, then
//! a 0. Members left out are at their default, so "missing" means "default".
//! UPDATEOBJ carries only what changed and is merged into the object.
//!
//! Object-typed members are a flexLong: 0 = null, 1 = an inline value struct
//! follows (type id, length, sparse fields), anything else = another object's
//! id. `DataList<T>` objects (the Core's lists) declare no members; their body
//! is `flexInt(count)` then `count` object ids.
//!
//! Object ids are handles that only mean something within this session.

use std::collections::HashMap;

use crate::frame::{push, Frame};
use crate::wire::{ReadResult, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum PropertyType {
    Int = 0,
    Long = 1,
    Bool = 2,
    Guid = 3,
    Sooid = 4,
    Double = 5,
    Float = 6,
    Char = 7,
    DateTime = 8,
    Enum = 9,
    NullableInt = 10,
    NullableLong = 11,
    NullableBool = 12,
    NullableGuid = 13,
    NullableSooid = 14,
    NullableDouble = 15,
    NullableFloat = 16,
    NullableChar = 17,
    NullableDateTime = 18,
    NullableEnum = 19,
    String = 20,
    ByteArray = 21,
    Message = 22,
    Object = 23,
    LengthPrefixed = 24,
}

impl PropertyType {
    pub fn from_wire(v: i32) -> Option<Self> {
        use PropertyType::*;
        Some(match v {
            0 => Int,
            1 => Long,
            2 => Bool,
            3 => Guid,
            4 => Sooid,
            5 => Double,
            6 => Float,
            7 => Char,
            8 => DateTime,
            9 => Enum,
            10 => NullableInt,
            11 => NullableLong,
            12 => NullableBool,
            13 => NullableGuid,
            14 => NullableSooid,
            15 => NullableDouble,
            16 => NullableFloat,
            17 => NullableChar,
            18 => NullableDateTime,
            19 => NullableEnum,
            20 => String,
            21 => ByteArray,
            22 => Message,
            23 => Object,
            24 => LengthPrefixed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TypeMember {
    pub name: String,
    pub prop_type: PropertyType,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub id: u32,
    pub name: String,
    pub members: Vec<TypeMember>,
}

/// A decoded field value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i32),
    Long(u64),
    Bool(bool),
    Double(f64),
    Float(f32),
    /// Sooids, GUIDs, byte arrays, messages and length-prefixed blobs.
    Bytes(Vec<u8>),
    Str(String),
    /// The id of another object in the graph.
    Ref(u64),
    /// A struct that came inline (by value).
    Inline(InlineStruct),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_ref_id(&self) -> Option<u64> {
        match self {
            Value::Ref(r) => Some(*r),
            _ => None,
        }
    }
    pub fn as_long(&self) -> Option<u64> {
        match self {
            Value::Long(v) => Some(*v),
            Value::Int(v) => Some(*v as u64),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Long(v) => Some(*v as i32),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InlineStruct {
    pub type_name: String,
    /// Keyed by the full member name the Core uses.
    pub fields: HashMap<String, Value>,
}

impl InlineStruct {
    /// A member by the end of its name ("Action" matches "...DeletePreview::Action").
    pub fn field(&self, suffix: &str) -> Option<&Value> {
        field_by_suffix(&self.fields, suffix)
    }
}

#[derive(Debug, Clone)]
pub struct RoonObject {
    pub oid: u64,
    pub type_id: u32,
    /// Fully qualified, e.g. "Sooloos.Broker.Api.TrackLite".
    pub type_name: String,
    /// Keyed by the full member name, e.g. "string Sooloos.Broker.Api.TrackLite::Title".
    pub fields: HashMap<String, Value>,
    /// `DataList<T>` only: the item object ids (items sent by reference).
    pub items: Option<Vec<u64>>,
    /// `DataList<T>` only: items sent by value (inline structs), in list order.
    pub structs: Option<Vec<InlineStruct>>,
}

impl RoonObject {
    /// A field by the end of its name, since names are fully qualified.
    pub fn field(&self, suffix: &str) -> Option<&Value> {
        field_by_suffix(&self.fields, suffix)
    }

    pub fn str_field(&self, suffix: &str) -> Option<&str> {
        self.field(suffix).and_then(Value::as_str)
    }

    pub fn ref_field(&self, suffix: &str) -> Option<u64> {
        self.field(suffix).and_then(Value::as_ref_id)
    }

    pub fn long_field(&self, suffix: &str) -> Option<u64> {
        self.field(suffix).and_then(Value::as_long)
    }

    pub fn has_field(&self, suffix: &str) -> bool {
        self.field(suffix).is_some()
    }

    /// The short type name ("TrackLite").
    pub fn short_type(&self) -> &str {
        self.type_name.rsplit('.').next().unwrap_or(&self.type_name)
    }
}

fn field_by_suffix<'a>(fields: &'a HashMap<String, Value>, suffix: &str) -> Option<&'a Value> {
    let wanted = format!("::{suffix}");
    fields.iter().find(|(k, _)| k.ends_with(&wanted)).map(|(_, v)| v)
}

fn is_data_list(type_name: &str) -> bool {
    type_name.starts_with("Sooloos.Broker.Api.DataList<") || type_name.contains(".DataList<")
}

#[derive(Default)]
pub struct ObjectGraph {
    pub types: HashMap<u32, TypeDef>,
    pub objects: HashMap<u64, RoonObject>,
}

impl ObjectGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one push frame. Returns the id of the object it created or
    /// changed, if it was an object frame.
    pub fn ingest(&mut self, frame: &Frame) -> Option<u64> {
        if frame.is_response {
            return None;
        }
        let mut r = Reader::new(&frame.body);
        match frame.cmd {
            push::DEFTYPE => {
                let _ = self.define_type(&mut r);
                None
            }
            push::PUSHOBJ | push::UPDATEOBJ => self.push_object(&mut r, true).ok(),
            push::PUSHSTUB => self.push_object(&mut r, false).ok(),
            _ => None,
        }
    }

    fn define_type(&mut self, r: &mut Reader) -> ReadResult<()> {
        let id = r.flex_int()?;
        let name = r.string()?.unwrap_or_default();
        let count = r.flex_int()?;
        let mut members = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let member_name = r.string()?.unwrap_or_default();
            let prop_type = PropertyType::from_wire(r.integer()?).unwrap_or(PropertyType::LengthPrefixed);
            members.push(TypeMember { name: member_name, prop_type });
        }
        self.types.insert(id, TypeDef { id, name, members });
        Ok(())
    }

    fn push_object(&mut self, r: &mut Reader, populate: bool) -> ReadResult<u64> {
        let oid = r.flex_long()?;
        let type_id = r.flex_int()?;
        let def = self.types.get(&type_id).cloned();
        let type_name = def.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| format!("#{type_id}"));
        let mut fields = HashMap::new();
        let mut items = None;
        let mut structs = None;

        if populate {
            if let Some(def) = &def {
                if is_data_list(&type_name) {
                    // Each item is an Object: a reference, or a struct by value.
                    let count = r.flex_int()?;
                    let mut list = Vec::with_capacity(count as usize);
                    let mut values = Vec::new();
                    for _ in 0..count {
                        if r.remaining() == 0 {
                            break;
                        }
                        match self.read_value(r, PropertyType::Object)? {
                            Value::Ref(oid) => list.push(oid),
                            Value::Inline(s) => values.push(s),
                            _ => {}
                        }
                    }
                    items = Some(list);
                    structs = Some(values);
                } else {
                    // A field we can't finish reading ends the object; keep what we have.
                    let _ = self.read_sparse_fields(r, def, &mut fields);
                }
            }
        }

        match self.objects.get_mut(&oid) {
            Some(existing) => {
                if populate {
                    existing.fields.extend(fields);
                    if items.is_some() {
                        existing.items = items;
                    }
                    if structs.is_some() {
                        existing.structs = structs;
                    }
                }
            }
            None => {
                self.objects.insert(
                    oid,
                    RoonObject {
                        oid,
                        type_id,
                        type_name,
                        fields,
                        items,
                        structs,
                    },
                );
            }
        }
        Ok(oid)
    }

    fn read_sparse_fields(&self, r: &mut Reader, def: &TypeDef, into: &mut HashMap<String, Value>) -> ReadResult<()> {
        loop {
            let idx = r.flex_int()?;
            if idx == 0 {
                return Ok(());
            }
            let Some(member) = def.members.get(idx as usize - 1) else {
                return Ok(()); // an index we don't know: we can't tell how long its value is
            };
            let value = self.read_value(r, member.prop_type)?;
            into.insert(member.name.clone(), value);
        }
    }

    /// Reads one value according to its PropertyType.
    pub fn read_value(&self, r: &mut Reader, t: PropertyType) -> ReadResult<Value> {
        use PropertyType as P;
        Ok(match t {
            P::Int | P::Enum | P::Char => Value::Int(r.integer()?),
            P::Long | P::DateTime => Value::Long(r.long()?),
            P::Bool => Value::Bool(r.boolean()?),
            P::Guid => Value::Bytes(r.guid()?.to_vec()),
            P::Sooid => Value::Bytes(r.sooid()?),
            P::Double => Value::Double(r.double()?),
            P::Float => Value::Float(r.float()?),
            P::NullableInt | P::NullableEnum | P::NullableChar => opt(r.optional_integer()?.map(Value::Int)),
            P::NullableLong | P::NullableDateTime => opt(r.optional_long()?.map(Value::Long)),
            P::NullableBool => opt(r.optional_boolean()?.map(Value::Bool)),
            P::NullableGuid => opt(r.optional_guid()?.map(|g| Value::Bytes(g.to_vec()))),
            P::NullableSooid => opt(r.optional_sooid()?.map(Value::Bytes)),
            P::NullableDouble => opt(r.optional_double()?.map(Value::Double)),
            P::NullableFloat => opt(r.optional_float()?.map(Value::Float)),
            P::String => opt(r.string()?.map(Value::Str)),
            P::ByteArray | P::Message => opt(r.byte_array()?.map(Value::Bytes)),
            P::LengthPrefixed => {
                let len = r.integer()?;
                if len < 0 {
                    Value::Null
                } else {
                    Value::Bytes(r.bytes(len as usize)?.to_vec())
                }
            }
            P::Object => {
                let marker = r.long()?;
                match marker {
                    0 => Value::Null,
                    1 => {
                        let type_id = r.flex_int()?;
                        let len = r.integer()?;
                        let body = r.bytes(len.max(0) as usize)?;
                        let mut sub = Reader::new(body);
                        let mut s = InlineStruct::default();
                        match self.types.get(&type_id) {
                            Some(def) => {
                                s.type_name = def.name.clone();
                                let _ = self.read_sparse_fields(&mut sub, def, &mut s.fields);
                            }
                            None => s.type_name = format!("#{type_id}"),
                        }
                        Value::Inline(s)
                    }
                    oid => Value::Ref(oid),
                }
            }
        })
    }

    /// Decodes a method's return value that used the Object encoding.
    pub fn decode_return_value(&self, payload: &[u8]) -> ReadResult<Value> {
        self.read_value(&mut Reader::new(payload), PropertyType::Object)
    }

    /// Decodes a returned list (`ResultCallback<IList<T>>`): flexInt(byte
    /// length), flexInt(count), then `count` Object-encoded values.
    pub fn decode_return_list(&self, payload: &[u8]) -> ReadResult<Vec<Value>> {
        let mut r = Reader::new(payload);
        r.flex_int()?;
        let count = r.flex_int()?;
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            out.push(self.read_value(&mut r, PropertyType::Object)?);
        }
        Ok(out)
    }

    pub fn get(&self, oid: u64) -> Option<&RoonObject> {
        self.objects.get(&oid)
    }

    /// The object a field points at, if it is a reference to one in the graph.
    pub fn deref(&self, value: Option<&Value>) -> Option<&RoonObject> {
        value.and_then(Value::as_ref_id).and_then(|oid| self.objects.get(&oid))
    }

    /// Objects whose type name is, or ends with ".", the given name.
    pub fn find_by_type<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a RoonObject> + 'a {
        let exact = name.to_string();
        let suffix = format!(".{name}");
        self.objects
            .values()
            .filter(move |o| o.type_name == exact || o.type_name.ends_with(&suffix))
    }

    pub fn first_of_type(&self, name: &str) -> Option<&RoonObject> {
        self.find_by_type(name).next()
    }
}

fn opt(v: Option<Value>) -> Value {
    v.unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Writer;

    fn hexbytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn frame(cmd: u8, body: Vec<u8>) -> Frame {
        Frame { is_response: false, cmd, rid: None, is_final: false, body }
    }

    fn deftype(g: &mut ObjectGraph, id: u32, name: &str, members: &[(&str, PropertyType)]) {
        let mut w = Writer::new();
        w.flex_int(id).string(Some(name)).flex_int(members.len() as u32);
        for (n, t) in members {
            w.string(Some(n)).integer(*t as i32);
        }
        g.ingest(&frame(push::DEFTYPE, w.into_bytes()));
    }

    #[test]
    fn objects_lists_and_updates() {
        let mut g = ObjectGraph::new();
        deftype(
            &mut g,
            10,
            "Sooloos.Broker.Api.TrackLite",
            &[
                ("string Sooloos.Broker.Api.TrackLite::Title", PropertyType::String),
                ("long Sooloos.Broker.Api.TrackLite::LibraryTrackId", PropertyType::Long),
                ("Sooloos.Broker.Api.AlbumLite Sooloos.Broker.Api.TrackLite::Album", PropertyType::Object),
                ("System.Byte[] Sooloos.Broker.Api.TrackLite::IsFavorite", PropertyType::ByteArray),
            ],
        );
        deftype(&mut g, 574, "Sooloos.Broker.Api.DataList<Sooloos.Broker.Api.Endpoint>", &[]); // 84 3e on the wire

        let mut w = Writer::new();
        w.long(2375439).flex_int(10).flex_int(1).string(Some("Collapsing Skies")).flex_int(3).long(34132).flex_int(4).byte_array(Some(&[0])).flex_int(0);
        assert_eq!(g.ingest(&frame(push::PUSHOBJ, w.into_bytes())), Some(2375439));
        let t = g.get(2375439).unwrap();
        assert_eq!(t.short_type(), "TrackLite");
        assert_eq!(t.str_field("Title"), Some("Collapsing Skies"));
        assert_eq!(t.ref_field("Album"), Some(34132));
        assert!(!t.has_field("LibraryTrackId"));

        // An update carrying only IsFavorite merges in
        let mut w = Writer::new();
        w.long(2375439).flex_int(10).flex_int(4).byte_array(Some(&hexbytes("01123f019657409adb84814f872dab89ff0b7f2f01"))).flex_int(0);
        g.ingest(&frame(push::UPDATEOBJ, w.into_bytes()));
        let t = g.get(2375439).unwrap();
        assert_eq!(t.str_field("Title"), Some("Collapsing Skies"));
        assert_eq!(t.field("IsFavorite").unwrap().as_bytes().unwrap().len(), 21);

        // DataList pushes exactly as the Core sent them
        g.ingest(&frame(push::PUSHOBJ, hexbytes("8193c45d843e00")));
        g.ingest(&frame(push::PUSHOBJ, hexbytes("8195c509843e038195c4358195c4538195c502")));
        assert_eq!(g.get(2417245).unwrap().items.as_deref(), Some(&[][..]));
        assert_eq!(g.get(2450057).unwrap().items.as_deref(), Some(&[2449973u64, 2450003, 2450050][..]));
    }

    #[test]
    fn return_list_as_captured_from_preview_delete() {
        let mut g = ObjectGraph::new();
        deftype(
            &mut g,
            216, // 81 58 on the wire
            "Sooloos.Broker.Api.DeletePreview",
            &[
                ("Sooloos.Broker.Api.DeleteAction Sooloos.Broker.Api.DeletePreview::Action", PropertyType::Enum),
                ("Sooloos.Broker.Api.TrackLite Sooloos.Broker.Api.DeletePreview::Track", PropertyType::Object),
                ("Sooloos.Broker.Api.AlbumLite Sooloos.Broker.Api.DeletePreview::Album", PropertyType::Object),
            ],
        );
        let list = g.decode_return_list(&hexbytes("0d0101815808010403819c975d00")).unwrap();
        assert_eq!(list.len(), 1);
        let Value::Inline(s) = &list[0] else { panic!("not inline") };
        assert_eq!(s.type_name, "Sooloos.Broker.Api.DeletePreview");
        assert_eq!(s.field("Action"), Some(&Value::Int(4)));
        assert_eq!(s.field("Album"), Some(&Value::Ref(2558941)));
        assert_eq!(s.field("Track"), None);
    }
}
