//! Little-endian Bedrock NBT (`level.dat`), hand-rolled. Names and strings are raw bytes
//! (never UTF-8 validated), compounds keep tag order and duplicates, an empty list keeps its
//! element type, floats are bit-exact: parse then write returns the original bytes.
//! Parsing never panics: every length is checked against the remaining input, depth is capped.

pub const END: u8 = 0;
pub const BYTE: u8 = 1;
pub const SHORT: u8 = 2;
pub const INT: u8 = 3;
pub const LONG: u8 = 4;
pub const FLOAT: u8 = 5;
pub const DOUBLE: u8 = 6;
pub const BYTE_ARRAY: u8 = 7;
pub const STRING: u8 = 8;
pub const LIST: u8 = 9;
pub const COMPOUND: u8 = 10;
pub const INT_ARRAY: u8 = 11;
pub const LONG_ARRAY: u8 = 12;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(Vec<u8>),
    /// Element tag id (kept when empty) and the elements, all of that type.
    List(u8, Vec<Tag>),
    Compound(Vec<(Vec<u8>, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => BYTE,
            Tag::Short(_) => SHORT,
            Tag::Int(_) => INT,
            Tag::Long(_) => LONG,
            Tag::Float(_) => FLOAT,
            Tag::Double(_) => DOUBLE,
            Tag::ByteArray(_) => BYTE_ARRAY,
            Tag::String(_) => STRING,
            Tag::List(..) => LIST,
            Tag::Compound(_) => COMPOUND,
            Tag::IntArray(_) => INT_ARRAY,
            Tag::LongArray(_) => LONG_ARRAY,
        }
    }

    pub fn string(s: &str) -> Tag {
        Tag::String(s.as_bytes().to_vec())
    }

    /// Integer value of a Byte/Short/Int/Long.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Tag::Byte(v) => Some(v.into()),
            Tag::Short(v) => Some(v.into()),
            Tag::Int(v) => Some(v.into()),
            Tag::Long(v) => Some(v),
            _ => None,
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.0.len() {
            return Err("truncated NBT".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }
    /// A non-negative `i32` element count whose elements (>= `unit` bytes each) still fit.
    fn count(&mut self, unit: usize) -> Result<usize, String> {
        let n = i32::from_le_bytes(self.arr()?);
        let n = usize::try_from(n).map_err(|_| "negative NBT length".to_string())?;
        if n.checked_mul(unit).is_none_or(|b| b > self.0.len()) {
            return Err("NBT length exceeds the input".into());
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<Vec<u8>, String> {
        let n = u16::from_le_bytes(self.arr()?);
        Ok(self.take(n.into())?.to_vec())
    }

    fn payload(&mut self, id: u8, depth: usize) -> Result<Tag, String> {
        if depth > MAX_DEPTH {
            return Err("NBT nested too deeply".into());
        }
        Ok(match id {
            BYTE => Tag::Byte(i8::from_le_bytes(self.arr()?)),
            SHORT => Tag::Short(i16::from_le_bytes(self.arr()?)),
            INT => Tag::Int(i32::from_le_bytes(self.arr()?)),
            LONG => Tag::Long(i64::from_le_bytes(self.arr()?)),
            FLOAT => Tag::Float(f32::from_bits(u32::from_le_bytes(self.arr()?))),
            DOUBLE => Tag::Double(f64::from_bits(u64::from_le_bytes(self.arr()?))),
            BYTE_ARRAY => {
                let n = self.count(1)?;
                Tag::ByteArray(self.take(n)?.to_vec())
            }
            STRING => Tag::String(self.string()?),
            LIST => {
                let elem = self.arr::<1>()?[0];
                let unit = match elem {
                    END => 0,
                    BYTE => 1,
                    SHORT | STRING => 2,
                    INT | FLOAT | BYTE_ARRAY | INT_ARRAY | LONG_ARRAY => 4,
                    LONG | DOUBLE => 8,
                    LIST => 5,
                    COMPOUND => 1,
                    _ => return Err(format!("unknown NBT list element type {elem}")),
                };
                let n = self.count(unit)?;
                if n > 0 && elem == END {
                    return Err("NBT list of End tags".into());
                }
                let items = (0..n)
                    .map(|_| self.payload(elem, depth + 1))
                    .collect::<Result<_, _>>()?;
                Tag::List(elem, items)
            }
            COMPOUND => {
                let mut v = Vec::new();
                loop {
                    let id = self.arr::<1>()?[0];
                    if id == END {
                        break Tag::Compound(v);
                    }
                    let name = self.string()?;
                    v.push((name, self.payload(id, depth + 1)?));
                }
            }
            INT_ARRAY => {
                let n = self.count(4)?;
                Tag::IntArray(
                    self.take(n * 4)?
                        .chunks_exact(4)
                        .map(|c| i32::from_le_bytes(c.try_into().expect("chunk of 4")))
                        .collect(),
                )
            }
            LONG_ARRAY => {
                let n = self.count(8)?;
                Tag::LongArray(
                    self.take(n * 8)?
                        .chunks_exact(8)
                        .map(|c| i64::from_le_bytes(c.try_into().expect("chunk of 8")))
                        .collect(),
                )
            }
            _ => return Err(format!("unknown NBT tag type {id}")),
        })
    }
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    // names and strings come from a parse (<= u16::MAX) or from short literals
    out.extend((s.len().min(u16::MAX as usize) as u16).to_le_bytes());
    out.extend(&s[..s.len().min(u16::MAX as usize)]);
}

fn write_payload(out: &mut Vec<u8>, t: &Tag) {
    match t {
        Tag::Byte(v) => out.extend(v.to_le_bytes()),
        Tag::Short(v) => out.extend(v.to_le_bytes()),
        Tag::Int(v) => out.extend(v.to_le_bytes()),
        Tag::Long(v) => out.extend(v.to_le_bytes()),
        Tag::Float(v) => out.extend(v.to_bits().to_le_bytes()),
        Tag::Double(v) => out.extend(v.to_bits().to_le_bytes()),
        Tag::ByteArray(v) => {
            out.extend((v.len() as i32).to_le_bytes());
            out.extend(v);
        }
        Tag::String(s) => put_str(out, s),
        Tag::List(elem, items) => {
            out.push(*elem);
            out.extend((items.len() as i32).to_le_bytes());
            items.iter().for_each(|i| write_payload(out, i));
        }
        Tag::Compound(v) => {
            for (name, t) in v {
                out.push(t.id());
                put_str(out, name);
                write_payload(out, t);
            }
            out.push(END);
        }
        Tag::IntArray(v) => {
            out.extend((v.len() as i32).to_le_bytes());
            v.iter().for_each(|i| out.extend(i.to_le_bytes()));
        }
        Tag::LongArray(v) => {
            out.extend((v.len() as i32).to_le_bytes());
            v.iter().for_each(|i| out.extend(i.to_le_bytes()));
        }
    }
}

/// `level.dat`: 8-byte header (storage version, payload length; both i32 LE), then the root
/// compound (type 10, empty name). The length must match the payload exactly.
pub fn parse_level_dat(data: &[u8]) -> Result<(i32, Tag), String> {
    if data.len() < 8 {
        return Err("level.dat shorter than its 8-byte header".into());
    }
    let version = i32::from_le_bytes(data[..4].try_into().expect("4 bytes"));
    let len = i32::from_le_bytes(data[4..8].try_into().expect("4 bytes"));
    if usize::try_from(len).ok() != Some(data.len() - 8) {
        return Err(format!(
            "level.dat header says {len} payload bytes, the file has {}",
            data.len() - 8
        ));
    }
    let mut r = Reader(&data[8..]);
    if r.arr::<1>()?[0] != COMPOUND {
        return Err("level.dat root is not a compound".into());
    }
    if !r.string()?.is_empty() {
        return Err("level.dat root has a name".into());
    }
    let root = r.payload(COMPOUND, 0)?;
    if !r.0.is_empty() {
        return Err("trailing bytes after the level.dat root".into());
    }
    Ok((version, root))
}

/// Inverse of [`parse_level_dat`]; the header length is recomputed. `root` must be a compound.
pub fn write_level_dat(version: i32, root: &Tag) -> Vec<u8> {
    let mut body = vec![COMPOUND, 0, 0];
    write_payload(&mut body, root);
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend(version.to_le_bytes());
    out.extend((body.len() as i32).to_le_bytes());
    out.extend(body);
    out
}

fn child<'a>(t: &'a Tag, name: &str) -> Option<&'a Tag> {
    match t {
        Tag::Compound(v) => v.iter().find(|(n, _)| n == name.as_bytes()).map(|(_, t)| t),
        _ => None,
    }
}

/// Tag at a dotted path (`experiments.cameras`) below a compound.
pub fn get<'a>(root: &'a Tag, path: &str) -> Option<&'a Tag> {
    path.split('.').try_fold(root, child)
}

pub fn get_mut<'a>(root: &'a mut Tag, path: &str) -> Option<&'a mut Tag> {
    let mut t = root;
    for seg in path.split('.') {
        let Tag::Compound(v) = t else { return None };
        t = &mut v.iter_mut().find(|(n, _)| n == seg.as_bytes())?.1;
    }
    Some(t)
}

/// Sets the tag at a dotted path, creating missing compounds on the way (new tags are
/// appended). Errors when a path segment is empty or an intermediate tag is not a compound.
pub fn set(root: &mut Tag, path: &str, value: Tag) -> Result<(), String> {
    let mut t = root;
    let mut segs = path.split('.').peekable();
    while let Some(seg) = segs.next() {
        if seg.is_empty() {
            return Err(format!("empty segment in NBT path {path:?}"));
        }
        let Tag::Compound(v) = t else {
            return Err(format!("{path:?}: a parent of {seg:?} is not a compound"));
        };
        let i = match v.iter().position(|(n, _)| n == seg.as_bytes()) {
            Some(i) => i,
            None => {
                v.push((seg.as_bytes().to_vec(), Tag::Compound(Vec::new())));
                v.len() - 1
            }
        };
        if segs.peek().is_none() {
            v[i].1 = value;
            return Ok(());
        }
        t = &mut v[i].1;
    }
    Err("empty NBT path".into())
}
