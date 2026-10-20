//! AMF0, only as much as an RTMP publisher's commands and metadata need.
//!
//! Decoding is bounded: nesting depth, the number of values in one message,
//! and every declared length is checked against the bytes actually present
//! before anything is allocated.

use std::fmt;

/// Deepest nesting of objects and arrays accepted.
pub const MAX_DEPTH: usize = 16;
/// Most values (counting nested ones) decoded from one message.
pub const MAX_VALUES: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    /// Properties in the order they arrived. Typed objects decode to this
    /// too (the class name is dropped).
    Object(Vec<(String, Value)>),
    Null,
    Undefined,
    /// An associative array; the declared count is advisory, as encoders
    /// often get it wrong, so it is read to the end marker like an object.
    EcmaArray(Vec<(String, Value)>),
    StrictArray(Vec<Value>),
    /// Milliseconds since the Unix epoch (the time zone field is ignored).
    Date(f64),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The properties of an object or ECMA array.
    pub fn properties(&self) -> Option<&[(String, Value)]> {
        match self {
            Self::Object(p) | Self::EcmaArray(p) => Some(p),
            _ => None,
        }
    }

    /// The first property called `key`, if this is an object or ECMA array.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.properties()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amf0Error {
    Truncated,
    UnsupportedMarker(u8),
    TooDeep,
    TooManyValues,
    BadEndMarker,
}

impl fmt::Display for Amf0Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("AMF0 value truncated"),
            Self::UnsupportedMarker(m) => write!(f, "unsupported AMF0 marker {m:#04x}"),
            Self::TooDeep => f.write_str("AMF0 nested too deeply"),
            Self::TooManyValues => f.write_str("too many AMF0 values"),
            Self::BadEndMarker => f.write_str("AMF0 object without an end marker"),
        }
    }
}

impl std::error::Error for Amf0Error {}

const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0a;
const DATE: u8 = 0x0b;
const LONG_STRING: u8 = 0x0c;
const UNSUPPORTED: u8 = 0x0d;
const XML_DOCUMENT: u8 = 0x0f;
const TYPED_OBJECT: u8 = 0x10;

/// Decodes every value in `bytes`, in order.
pub fn decode_all(bytes: &[u8]) -> Result<Vec<Value>, Amf0Error> {
    let mut d = Decoder {
        bytes,
        pos: 0,
        values: 0,
    };
    let mut out = Vec::new();
    while d.pos < bytes.len() {
        out.push(d.value(0)?);
    }
    Ok(out)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    values: usize,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Amf0Error> {
        let end = self.pos.checked_add(n).ok_or(Amf0Error::Truncated)?;
        let s = self.bytes.get(self.pos..end).ok_or(Amf0Error::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, Amf0Error> {
        let [b] = self.take(1)? else {
            return Err(Amf0Error::Truncated);
        };
        Ok(*b)
    }

    fn u16(&mut self) -> Result<u16, Amf0Error> {
        let [a, b] = self.take(2)? else {
            return Err(Amf0Error::Truncated);
        };
        Ok(u16::from_be_bytes([*a, *b]))
    }

    fn u32(&mut self) -> Result<u32, Amf0Error> {
        let [a, b, c, d] = self.take(4)? else {
            return Err(Amf0Error::Truncated);
        };
        Ok(u32::from_be_bytes([*a, *b, *c, *d]))
    }

    fn f64(&mut self) -> Result<f64, Amf0Error> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(f64::from_be_bytes(a))
    }

    fn string(&mut self, len: usize) -> Result<String, Amf0Error> {
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn short_string(&mut self) -> Result<String, Amf0Error> {
        let len = usize::from(self.u16()?);
        self.string(len)
    }

    fn long_string(&mut self) -> Result<String, Amf0Error> {
        let len = usize::try_from(self.u32()?).map_err(|_| Amf0Error::Truncated)?;
        self.string(len)
    }

    fn value(&mut self, depth: usize) -> Result<Value, Amf0Error> {
        self.values += 1;
        if self.values > MAX_VALUES {
            return Err(Amf0Error::TooManyValues);
        }
        let marker = self.u8()?;
        Ok(match marker {
            NUMBER => Value::Number(self.f64()?),
            BOOLEAN => Value::Boolean(self.u8()? != 0),
            STRING => Value::String(self.short_string()?),
            LONG_STRING | XML_DOCUMENT => Value::String(self.long_string()?),
            NULL => Value::Null,
            UNDEFINED | UNSUPPORTED => Value::Undefined,
            OBJECT => Value::Object(self.properties(depth)?),
            TYPED_OBJECT => {
                let _class = self.short_string()?;
                Value::Object(self.properties(depth)?)
            }
            ECMA_ARRAY => {
                let _count = self.u32()?;
                Value::EcmaArray(self.properties(depth)?)
            }
            STRICT_ARRAY => {
                if depth >= MAX_DEPTH {
                    return Err(Amf0Error::TooDeep);
                }
                let count = self.u32()?;
                // Every value takes at least one byte: a count beyond the
                // bytes left is a lie, refused before looping on it.
                let left = self.bytes.len().saturating_sub(self.pos);
                if usize::try_from(count).map_or(true, |c| c > left) {
                    return Err(Amf0Error::Truncated);
                }
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Value::StrictArray(items)
            }
            DATE => {
                let ms = self.f64()?;
                let _tz = self.u16()?;
                Value::Date(ms)
            }
            other => return Err(Amf0Error::UnsupportedMarker(other)),
        })
    }

    /// Key/value pairs up to the `00 00 09` end marker.
    fn properties(&mut self, depth: usize) -> Result<Vec<(String, Value)>, Amf0Error> {
        if depth >= MAX_DEPTH {
            return Err(Amf0Error::TooDeep);
        }
        let mut props = Vec::new();
        loop {
            let key = self.short_string()?;
            if key.is_empty() {
                // An empty key must be followed by the end marker.
                return match self.u8()? {
                    OBJECT_END => Ok(props),
                    _ => Err(Amf0Error::BadEndMarker),
                };
            }
            let v = self.value(depth + 1)?;
            props.push((key, v));
        }
    }
}

/// Appends the encoding of `v` to `out`.
pub fn encode(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Number(n) => {
            out.push(NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Value::Boolean(b) => out.extend_from_slice(&[BOOLEAN, u8::from(*b)]),
        Value::String(s) => match u16::try_from(s.len()) {
            Ok(len) => {
                out.push(STRING);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            Err(_) => {
                out.push(LONG_STRING);
                let len = u32::try_from(s.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(s.as_bytes().get(..len as usize).unwrap_or_default());
            }
        },
        Value::Object(props) => {
            out.push(OBJECT);
            encode_properties(props, out);
        }
        Value::Null => out.push(NULL),
        Value::Undefined => out.push(UNDEFINED),
        Value::EcmaArray(props) => {
            out.push(ECMA_ARRAY);
            let count = u32::try_from(props.len()).unwrap_or(u32::MAX);
            out.extend_from_slice(&count.to_be_bytes());
            encode_properties(props, out);
        }
        Value::StrictArray(items) => {
            out.push(STRICT_ARRAY);
            let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
            out.extend_from_slice(&count.to_be_bytes());
            for item in items {
                encode(item, out);
            }
        }
        Value::Date(ms) => {
            out.push(DATE);
            out.extend_from_slice(&ms.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
        }
    }
}

fn encode_properties(props: &[(String, Value)], out: &mut Vec<u8>) {
    for (k, v) in props {
        // Keys are short by nature; one too long for the field is cut, on
        // a character boundary.
        let mut end = k.len().min(usize::from(u16::MAX));
        while !k.is_char_boundary(end) {
            end -= 1;
        }
        let key = k.get(..end).unwrap_or_default().as_bytes();
        out.extend_from_slice(&(key.len() as u16).to_be_bytes());
        out.extend_from_slice(key);
        encode(v, out);
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: Value) {
        let mut b = Vec::new();
        encode(&v, &mut b);
        assert_eq!(decode_all(&b), Ok(vec![v]));
    }

    #[test]
    fn values_round_trip() {
        round_trip(Value::Number(1.5));
        round_trip(Value::Number(-0.0));
        round_trip(Value::Boolean(true));
        round_trip(Value::Boolean(false));
        round_trip(Value::String(String::new()));
        round_trip(Value::String("connect".into()));
        round_trip(Value::String("x".repeat(70_000)));
        round_trip(Value::Null);
        round_trip(Value::Undefined);
        // A key past the 16-bit length is cut on a character boundary.
        let mut b = Vec::new();
        let long = "é".repeat(40_000);
        encode(&Value::Object(vec![(long, Value::Null)]), &mut b);
        let Ok(v) = decode_all(&b) else {
            panic!("decodes");
        };
        assert_eq!(v[0].properties().map(|p| p[0].0.len()), Some(65_534));
        round_trip(Value::Date(1_700_000_000_000.0));
        round_trip(Value::Object(vec![
            ("app".into(), Value::String("live".into())),
            (
                "nested".into(),
                Value::Object(vec![("n".into(), Value::Number(3.0))]),
            ),
        ]));
        round_trip(Value::EcmaArray(vec![(
            "audiocodecid".into(),
            Value::Number(10.0),
        )]));
        round_trip(Value::StrictArray(vec![
            Value::Null,
            Value::Number(1.0),
            Value::StrictArray(vec![]),
        ]));
    }

    #[test]
    fn decodes_a_real_connect_command() {
        // The start of ffmpeg's connect, as recorded.
        let mut b = Vec::new();
        for v in [
            Value::String("connect".into()),
            Value::Number(1.0),
            Value::Object(vec![
                ("app".into(), Value::String("live".into())),
                ("type".into(), Value::String("nonprivate".into())),
            ]),
        ] {
            encode(&v, &mut b);
        }
        let vals = decode_all(&b).expect("decodes");
        assert_eq!(vals[0].as_str(), Some("connect"));
        assert_eq!(vals[1].as_number(), Some(1.0));
        assert_eq!(vals[2].get("app").and_then(Value::as_str), Some("live"));
    }

    #[test]
    fn ecma_array_count_is_advisory() {
        // Declares 7 entries, holds 1: encoders get this wrong.
        let mut b = vec![ECMA_ARRAY, 0, 0, 0, 7, 0, 1, b'a'];
        encode(&Value::Number(2.0), &mut b);
        b.extend_from_slice(&[0, 0, OBJECT_END]);
        assert_eq!(
            decode_all(&b),
            Ok(vec![Value::EcmaArray(vec![(
                "a".into(),
                Value::Number(2.0)
            )])])
        );
    }

    #[test]
    fn typed_object_and_long_string() {
        let mut b = vec![TYPED_OBJECT, 0, 1, b'C', 0, 1, b'k'];
        b.extend_from_slice(&[LONG_STRING, 0, 0, 0, 2, b'h', b'i']);
        b.extend_from_slice(&[0, 0, OBJECT_END]);
        assert_eq!(
            decode_all(&b),
            Ok(vec![Value::Object(vec![(
                "k".into(),
                Value::String("hi".into())
            )])])
        );
    }

    #[test]
    fn refuses_hostile_input() {
        assert_eq!(decode_all(&[NUMBER, 0, 0]), Err(Amf0Error::Truncated));
        assert_eq!(
            decode_all(&[STRING, 0xff, 0xff, b'a']),
            Err(Amf0Error::Truncated)
        );
        assert_eq!(
            decode_all(&[LONG_STRING, 0xff, 0xff, 0xff, 0xff]),
            Err(Amf0Error::Truncated)
        );
        // A strict array claiming four billion entries.
        assert_eq!(
            decode_all(&[STRICT_ARRAY, 0xff, 0xff, 0xff, 0xff, NULL]),
            Err(Amf0Error::Truncated)
        );
        assert_eq!(
            decode_all(&[OBJECT, 0, 0, 0x07]),
            Err(Amf0Error::BadEndMarker)
        );
        assert_eq!(decode_all(&[OBJECT, 0, 1, b'a']), Err(Amf0Error::Truncated));
        assert_eq!(decode_all(&[0x11]), Err(Amf0Error::UnsupportedMarker(0x11)));
        // Deep nesting.
        let mut deep = Vec::new();
        for _ in 0..100 {
            deep.extend_from_slice(&[OBJECT, 0, 1, b'a']);
        }
        assert_eq!(decode_all(&deep), Err(Amf0Error::TooDeep));
        let mut deep = Vec::new();
        for _ in 0..100 {
            deep.extend_from_slice(&[STRICT_ARRAY, 0, 0, 0, 1]);
        }
        deep.push(NULL);
        assert_eq!(decode_all(&deep), Err(Amf0Error::TooDeep));
        // Too many values.
        assert_eq!(
            decode_all(&vec![NULL; MAX_VALUES + 1]),
            Err(Amf0Error::TooManyValues)
        );
        assert_eq!(
            decode_all(&vec![NULL; MAX_VALUES]).map(|v| v.len()),
            Ok(MAX_VALUES)
        );
        // Invalid UTF-8 is replaced, not refused.
        assert_eq!(
            decode_all(&[STRING, 0, 1, 0xff]),
            Ok(vec![Value::String("\u{fffd}".into())])
        );
    }
}
