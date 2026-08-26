//! A minimal binary FBX 7.4 writer.
//!
//! Only what a static, textured mesh scene needs: geometry, models, materials,
//! textures and the connections between them. No animation, no skinning, no
//! deformers.
//!
//! Version 7400 is deliberate. From 7500 on, record offsets become 64-bit, and
//! while that lifts the 2 GB ceiling it also loses a handful of older importers
//! for no benefit here.
//!
//! Array properties are written uncompressed. Deflate would roughly halve the
//! file, but this runs on every export and the point of doing it in-process is
//! speed; the caller can zip the folder if size matters.

use std::io::{Result, Seek, Write};

pub struct Node {
    name: &'static str,
    props: Vec<Prop>,
    children: Vec<Node>,
}

enum Prop {
    Bool(u8),
    I32(i32),
    F64(f64),
    I64(i64),
    Str(String),
    Raw(Vec<u8>),
    /// Already encoded: type code, element count, encoding flag and payload.
    /// Baking it here rather than at write time keeps record sizes computable
    /// without compressing twice.
    Array { code: u8, count: u32, encoding: u32, payload: Vec<u8> },
}

/// Arrays go out deflated. Uncompressed is marginally faster to write but the
/// files are roughly three times the size - 113 MB against 39 MB for one truck -
/// and the compression cost is small next to reading the archive.
fn encode_array(code: u8, count: usize, raw: Vec<u8>) -> Prop {
    use flate2::{write::ZlibEncoder, Compression};
    let mut e = ZlibEncoder::new(Vec::with_capacity(raw.len() / 2), Compression::fast());
    let ok = e.write_all(&raw).is_ok();
    match (ok, e.finish()) {
        (true, Ok(z)) if z.len() < raw.len() => Prop::Array {
            code,
            count: count as u32,
            encoding: 1,
            payload: z,
        },
        _ => Prop::Array { code, count: count as u32, encoding: 0, payload: raw },
    }
}

impl Node {
    pub fn new(name: &'static str) -> Self {
        Node { name, props: Vec::new(), children: Vec::new() }
    }
    pub fn bool(mut self, v: bool) -> Self { self.props.push(Prop::Bool(v as u8)); self }
    pub fn i32(mut self, v: i32) -> Self { self.props.push(Prop::I32(v)); self }
    pub fn f64(mut self, v: f64) -> Self { self.props.push(Prop::F64(v)); self }
    pub fn i64(mut self, v: i64) -> Self { self.props.push(Prop::I64(v)); self }
    pub fn str(mut self, v: impl Into<String>) -> Self { self.props.push(Prop::Str(v.into())); self }
    pub fn raw(mut self, v: Vec<u8>) -> Self { self.props.push(Prop::Raw(v)); self }
    pub fn arr_f64(mut self, v: Vec<f64>) -> Self {
        let mut raw = Vec::with_capacity(v.len() * 8);
        for x in &v {
            raw.extend_from_slice(&x.to_le_bytes());
        }
        self.props.push(encode_array(b'd', v.len(), raw));
        self
    }
    pub fn arr_i32(mut self, v: Vec<i32>) -> Self {
        let mut raw = Vec::with_capacity(v.len() * 4);
        for x in &v {
            raw.extend_from_slice(&x.to_le_bytes());
        }
        self.props.push(encode_array(b'i', v.len(), raw));
        self
    }
    pub fn child(mut self, c: Node) -> Self { self.children.push(c); self }
    pub fn children(mut self, cs: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(cs);
        self
    }

    /// An FBX object name is stored as `Class::Name` with a NUL and a 0x01
    /// between the two halves, not with the literal colons that the ASCII form
    /// shows.
    pub fn objname(self, class: &str, name: &str) -> Self {
        let mut b = Vec::with_capacity(class.len() + name.len() + 2);
        b.extend_from_slice(name.as_bytes());
        b.push(0x00);
        b.push(0x01);
        b.extend_from_slice(class.as_bytes());
        self.raw(b)
    }
}

fn write_prop<W: Write>(w: &mut W, p: &Prop) -> Result<()> {
    match p {
        Prop::Bool(v) => { w.write_all(b"C")?; w.write_all(&[*v])?; }
        Prop::I32(v) => { w.write_all(b"I")?; w.write_all(&v.to_le_bytes())?; }
        Prop::F64(v) => { w.write_all(b"D")?; w.write_all(&v.to_le_bytes())?; }
        Prop::I64(v) => { w.write_all(b"L")?; w.write_all(&v.to_le_bytes())?; }
        Prop::Str(s) => {
            w.write_all(b"S")?;
            w.write_all(&(s.len() as u32).to_le_bytes())?;
            w.write_all(s.as_bytes())?;
        }
        Prop::Raw(b) => {
            w.write_all(b"S")?;
            w.write_all(&(b.len() as u32).to_le_bytes())?;
            w.write_all(b)?;
        }
        Prop::Array { code, count, encoding, payload } => {
            w.write_all(&[*code])?;
            w.write_all(&count.to_le_bytes())?;
            w.write_all(&encoding.to_le_bytes())?;
            w.write_all(&(payload.len() as u32).to_le_bytes())?;
            w.write_all(payload)?;
        }
    }
    Ok(())
}

fn prop_len(p: &Prop) -> usize {
    1 + match p {
        Prop::Bool(_) => 1,
        Prop::I32(_) => 4,
        Prop::F64(_) | Prop::I64(_) => 8,
        Prop::Str(s) => 4 + s.len(),
        Prop::Raw(b) => 4 + b.len(),
        Prop::Array { payload, .. } => 12 + payload.len(),
    }
}

/// Size of a record including its header, so end offsets can be written without
/// seeking back over multi-megabyte arrays.
fn node_len(n: &Node) -> usize {
    let props: usize = n.props.iter().map(prop_len).sum();
    let mut total = 13 + n.name.len() + props;
    if !n.children.is_empty() {
        total += n.children.iter().map(node_len).sum::<usize>() + 13;
    }
    total
}

fn write_node<W: Write>(w: &mut W, n: &Node, at: usize) -> Result<usize> {
    let len = node_len(n);
    let end = at + len;
    let props_len: usize = n.props.iter().map(prop_len).sum();
    w.write_all(&(end as u32).to_le_bytes())?;
    w.write_all(&(n.props.len() as u32).to_le_bytes())?;
    w.write_all(&(props_len as u32).to_le_bytes())?;
    w.write_all(&[n.name.len() as u8])?;
    w.write_all(n.name.as_bytes())?;
    for p in &n.props {
        write_prop(w, p)?;
    }
    let mut pos = at + 13 + n.name.len() + props_len;
    if !n.children.is_empty() {
        for c in &n.children {
            pos = write_node(w, c, pos)?;
        }
        w.write_all(&[0u8; 13])?;   // nested-list terminator
        pos += 13;
    }
    debug_assert_eq!(pos, end);
    Ok(pos)
}

pub fn write<W: Write + Seek>(w: &mut W, roots: &[Node]) -> Result<()> {
    w.write_all(b"Kaydara FBX Binary  \x00")?;
    w.write_all(&[0x1A, 0x00])?;
    w.write_all(&7400u32.to_le_bytes())?;
    let mut pos = 27usize;
    for n in roots {
        pos = write_node(w, n, pos)?;
    }
    w.write_all(&[0u8; 13])?;       // end of the top-level list

    // Footer. The 16-byte id and the "extension" block are what importers look
    // for; the padding aligns the whole file to 16 bytes as the format expects.
    w.write_all(&[0u8; 16])?;
    let here = w.stream_position()? as usize;
    let pad = (16 - (here % 16)) % 16;
    w.write_all(&vec![0u8; pad])?;
    w.write_all(&0u32.to_le_bytes())?;
    w.write_all(&7400u32.to_le_bytes())?;
    w.write_all(&[0u8; 120])?;
    w.write_all(&[
        0xF8, 0x5A, 0x8C, 0x6A, 0xDE, 0xF5, 0xD9, 0x7E,
        0xEC, 0xE9, 0x0C, 0xE3, 0x75, 0x8F, 0x29, 0x0B,
    ])?;
    let _ = pos;
    Ok(())
}

/// FBX ties objects together by 64-bit id. Any distinct non-zero value works;
/// a counter keeps them stable between runs, which matters for diffing output.
pub struct Ids(i64);

impl Default for Ids {
    fn default() -> Self {
        Ids(1_000_000)
    }
}

impl Ids {
    pub fn next(&mut self) -> i64 {
        self.0 += 1;
        self.0
    }
}

/// `Properties70` entries, the FBX way of carrying named values.
pub fn p70_d(name: &str, ty: &str, sub: &str, v: f64) -> Node {
    Node::new("P").str(name).str(ty).str(sub).str("").f64(v)
}

pub fn p70_i(name: &str, ty: &str, sub: &str, v: i32) -> Node {
    Node::new("P").str(name).str(ty).str(sub).str("").i32(v)
}

pub fn p70_c3(name: &str, ty: &str, sub: &str, v: [f64; 3]) -> Node {
    Node::new("P").str(name).str(ty).str(sub).str("").f64(v[0]).f64(v[1]).f64(v[2])
}

pub fn p70_s(name: &str, ty: &str, sub: &str, v: &str) -> Node {
    Node::new("P").str(name).str(ty).str(sub).str("").str(v)
}



