// SPDX-License-Identifier: MIT OR Apache-2.0
//! D-Bus wire format: signatures, values, and messages.
//!
//! Written for two jobs: to build the few calls the outer stub makes, and to read messages that
//! are not trusted (the replies of the bus today, messages that come from the app later). So the
//! reader is strict: every length is checked against a limit before anything is allocated, nesting
//! is bounded, padding must be zero, strings must be valid UTF-8 without NUL, object paths and
//! signatures must be well formed, and a file descriptor index must name a descriptor that arrived.
//! It never panics on any input (`tests` and the fuzz target `fuzz_dbus` check that).

use anyhow::{bail, ensure, Result};

/// Limits applied while reading. The defaults are far below the protocol maximum (128 MiB).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Whole message: fixed header, header fields and body.
    pub max_message: usize,
    /// Header fields array.
    pub max_header_fields: usize,
    /// One string, object path or signature.
    pub max_string: usize,
    /// Bytes of one array.
    pub max_array_bytes: usize,
    /// Elements of one array.
    pub max_array_elems: usize,
    /// Nesting of arrays, structs, dict entries and variants together.
    pub max_depth: usize,
    /// Descriptors that may come with one message.
    pub max_fds: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_message: 1 << 20, max_header_fields: 4096, max_string: 1 << 16, max_array_bytes: 1 << 19, max_array_elems: 4096, max_depth: 32, max_fds: 16 }
    }
}

/// A value of the types the stub needs. A `Dict` is an array of dict entries.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Path(String),
    Sig(String),
    /// Index into the descriptors that travel with the message.
    Fd(u32),
    /// Element signature, then the elements.
    Array(String, Vec<Value>),
    Struct(Vec<Value>),
    /// Key (a basic type) and value.
    Entry(Box<Value>, Box<Value>),
    Variant(Box<Value>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }
    pub fn variant(v: Value) -> Value {
        Value::Variant(Box::new(v))
    }
    /// The signature of this value.
    pub fn signature(&self) -> String {
        match self {
            Value::Byte(_) => "y".into(),
            Value::Bool(_) => "b".into(),
            Value::I16(_) => "n".into(),
            Value::U16(_) => "q".into(),
            Value::I32(_) => "i".into(),
            Value::U32(_) => "u".into(),
            Value::I64(_) => "x".into(),
            Value::U64(_) => "t".into(),
            Value::F64(_) => "d".into(),
            Value::Str(_) => "s".into(),
            Value::Path(_) => "o".into(),
            Value::Sig(_) => "g".into(),
            Value::Fd(_) => "h".into(),
            Value::Array(e, _) => format!("a{e}"),
            Value::Struct(v) => format!("({})", v.iter().map(Value::signature).collect::<String>()),
            Value::Entry(k, v) => format!("{{{}{}}}", k.signature(), v.signature()),
            Value::Variant(_) => "v".into(),
        }
    }
}

// ---- signatures ----

/// End of the single complete type that starts at `pos` in `sig`. `arrays` and `structs` count the
/// nesting already open (the protocol allows 32 of each).
fn type_end(sig: &[u8], pos: usize, arrays: usize, structs: usize) -> Result<usize> {
    let c = *sig.get(pos).ok_or_else(|| anyhow::anyhow!("signature ends inside a type"))?;
    match c {
        b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o' | b'g' | b'h' | b'v' => Ok(pos + 1),
        b'a' => {
            ensure!(arrays < 32, "signature: arrays nested too deep");
            if sig.get(pos + 1) == Some(&b'{') {
                ensure!(structs < 32, "signature: structs nested too deep");
                let key = *sig.get(pos + 2).ok_or_else(|| anyhow::anyhow!("signature ends inside a dict entry"))?;
                ensure!(b"ybnqiuxtdsogh".contains(&key), "signature: dict key must be a basic type");
                let v_end = type_end(sig, pos + 3, arrays + 1, structs + 1)?;
                ensure!(sig.get(v_end) == Some(&b'}'), "signature: dict entry must have exactly a key and a value");
                Ok(v_end + 1)
            } else {
                type_end(sig, pos + 1, arrays + 1, structs)
            }
        }
        b'(' => {
            ensure!(structs < 32, "signature: structs nested too deep");
            let mut p = pos + 1;
            let mut n = 0;
            while sig.get(p) != Some(&b')') {
                p = type_end(sig, p, arrays, structs + 1)?;
                n += 1;
            }
            ensure!(n > 0, "signature: empty struct");
            Ok(p + 1)
        }
        _ => bail!("signature: unknown type character {:?}", c as char),
    }
}

/// A signature of zero or more complete types (the signature of a body).
pub fn check_signature(sig: &str) -> Result<()> {
    ensure!(sig.len() <= 255, "signature longer than 255 bytes");
    let b = sig.as_bytes();
    let mut p = 0;
    while p < b.len() {
        p = type_end(b, p, 0, 0)?;
    }
    Ok(())
}

/// The signature of the element of an array: one complete type, or a dict entry `{kv}`.
pub fn check_elem(e: &str) -> Result<()> {
    check_single(&format!("a{e}"))
}

/// A signature of exactly one complete type.
pub fn check_single(sig: &str) -> Result<()> {
    ensure!(sig.len() <= 255, "signature longer than 255 bytes");
    ensure!(type_end(sig.as_bytes(), 0, 0, 0)? == sig.len(), "signature: more than one complete type");
    Ok(())
}

/// The complete types of a signature, one string each.
pub fn split_signature(sig: &str) -> Result<Vec<&str>> {
    check_signature(sig)?;
    let b = sig.as_bytes();
    let mut v = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let e = type_end(b, p, 0, 0)?;
        v.push(&sig[p..e]);
        p = e;
    }
    Ok(v)
}

fn alignment(c: u8) -> usize {
    match c {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'a' | b'h' => 4,
        _ => 8,
    }
}

/// Valid object path: `/`, or `/` followed by elements of `[A-Za-z0-9_]+` separated by `/`.
pub fn valid_object_path(p: &str) -> bool {
    if p == "/" {
        return true;
    }
    p.starts_with('/') && p[1..].split('/').all(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

// ---- writer (little endian) ----

pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { buf: Vec::new() }
    }
    fn pad(&mut self, n: usize) {
        while !self.buf.len().is_multiple_of(n) {
            self.buf.push(0);
        }
    }
    pub fn value(&mut self, v: &Value) -> Result<()> {
        match v {
            Value::Byte(x) => self.buf.push(*x),
            Value::Bool(x) => {
                self.pad(4);
                self.buf.extend_from_slice(&u32::from(*x).to_le_bytes());
            }
            Value::I16(x) => {
                self.pad(2);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U16(x) => {
                self.pad(2);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::I32(x) => {
                self.pad(4);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U32(x) | Value::Fd(x) => {
                self.pad(4);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::I64(x) => {
                self.pad(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::U64(x) => {
                self.pad(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::F64(x) => {
                self.pad(8);
                self.buf.extend_from_slice(&x.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => {
                ensure!(!s.as_bytes().contains(&0), "string with a NUL byte");
                if matches!(v, Value::Path(_)) {
                    ensure!(valid_object_path(s), "invalid object path");
                }
                ensure!(s.len() < u32::MAX as usize, "string too long");
                self.pad(4);
                self.buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
            }
            Value::Sig(s) => {
                check_signature(s)?;
                self.buf.push(s.len() as u8);
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
            }
            Value::Array(e, items) => {
                check_elem(e)?;
                for it in items {
                    ensure!(it.signature() == *e, "array element has signature {} instead of {e}", it.signature());
                }
                self.pad(4);
                let len_at = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                self.pad(alignment(e.as_bytes()[0]));
                let start = self.buf.len();
                for it in items {
                    self.value(it)?;
                }
                let len = self.buf.len() - start;
                ensure!(len <= 1 << 26, "array too long");
                self.buf[len_at..len_at + 4].copy_from_slice(&(len as u32).to_le_bytes());
            }
            Value::Struct(items) => {
                ensure!(!items.is_empty(), "empty struct");
                self.pad(8);
                for it in items {
                    self.value(it)?;
                }
            }
            Value::Entry(k, val) => {
                self.pad(8);
                self.value(k)?;
                self.value(val)?;
            }
            Value::Variant(inner) => {
                let s = inner.signature();
                check_single(&s)?;
                self.buf.push(s.len() as u8);
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
                self.value(inner)?;
            }
        }
        Ok(())
    }
}

impl Default for Writer {
    fn default() -> Self {
        Writer::new()
    }
}

// ---- reader (either byte order) ----

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    big: bool,
    nfds: usize,
    lim: Limits,
}

impl<'a> Reader<'a> {
    /// `buf` is the whole message; `pos` is where the data to read starts, so that alignment is
    /// relative to the start of the message, as the protocol says.
    pub fn new(buf: &'a [u8], pos: usize, big: bool, nfds: usize, lim: Limits) -> Reader<'a> {
        Reader { buf, pos, big, nfds, lim }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len());
        let end = end.ok_or_else(|| anyhow::anyhow!("message is shorter than its contents say"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn align(&mut self, n: usize) -> Result<()> {
        let rem = self.pos % n;
        if rem != 0 {
            let pad = self.take(n - rem)?;
            ensure!(pad.iter().all(|b| *b == 0), "padding is not zero");
        }
        Ok(())
    }
    fn u16(&mut self) -> Result<u16> {
        self.align(2)?;
        let b: [u8; 2] = self.take(2)?.try_into()?;
        Ok(if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }
    fn u32(&mut self) -> Result<u32> {
        self.align(4)?;
        let b: [u8; 4] = self.take(4)?.try_into()?;
        Ok(if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }
    fn u64(&mut self) -> Result<u64> {
        self.align(8)?;
        let b: [u8; 8] = self.take(8)?.try_into()?;
        Ok(if self.big { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) })
    }
    fn string_body(&mut self, len: usize) -> Result<String> {
        ensure!(len <= self.lim.max_string, "string longer than the limit ({len} bytes)");
        let b = self.take(len)?;
        ensure!(self.take(1)?[0] == 0, "string is not NUL-terminated");
        ensure!(!b.contains(&0), "string with a NUL byte");
        Ok(std::str::from_utf8(b).map_err(|_| anyhow::anyhow!("string is not UTF-8"))?.to_string())
    }
    /// Read one value of the single complete type `sig`.
    pub fn value(&mut self, sig: &str, depth: usize) -> Result<Value> {
        ensure!(depth <= self.lim.max_depth, "values nested deeper than the limit");
        let b = sig.as_bytes();
        match b[0] {
            b'y' => Ok(Value::Byte(self.take(1)?[0])),
            b'b' => match self.u32()? {
                0 => Ok(Value::Bool(false)),
                1 => Ok(Value::Bool(true)),
                n => bail!("boolean with value {n}"),
            },
            b'n' => Ok(Value::I16(self.u16()? as i16)),
            b'q' => Ok(Value::U16(self.u16()?)),
            b'i' => Ok(Value::I32(self.u32()? as i32)),
            b'u' => Ok(Value::U32(self.u32()?)),
            b'x' => Ok(Value::I64(self.u64()? as i64)),
            b't' => Ok(Value::U64(self.u64()?)),
            b'd' => Ok(Value::F64(f64::from_bits(self.u64()?))),
            b'h' => {
                let i = self.u32()?;
                ensure!((i as usize) < self.nfds, "file descriptor index {i} but {} arrived", self.nfds);
                Ok(Value::Fd(i))
            }
            b's' => {
                let n = self.u32()? as usize;
                Ok(Value::Str(self.string_body(n)?))
            }
            b'o' => {
                let n = self.u32()? as usize;
                let s = self.string_body(n)?;
                ensure!(valid_object_path(&s), "invalid object path");
                Ok(Value::Path(s))
            }
            b'g' => {
                let n = self.take(1)?[0] as usize;
                let s = self.string_body(n)?;
                check_signature(&s)?;
                Ok(Value::Sig(s))
            }
            b'v' => {
                let n = self.take(1)?[0] as usize;
                let s = self.string_body(n)?;
                check_single(&s)?;
                Ok(Value::Variant(Box::new(self.value(&s, depth + 1)?)))
            }
            b'a' => {
                let elem = &sig[1..];
                let len = self.u32()? as usize;
                ensure!(len <= self.lim.max_array_bytes, "array longer than the limit ({len} bytes)");
                self.align(alignment(elem.as_bytes()[0]))?;
                let end = self.pos.checked_add(len).filter(|e| *e <= self.buf.len());
                let end = end.ok_or_else(|| anyhow::anyhow!("array runs past the end of the message"))?;
                let mut items = Vec::new();
                while self.pos < end {
                    ensure!(items.len() < self.lim.max_array_elems, "array with more elements than the limit");
                    items.push(self.value(elem, depth + 1)?);
                }
                ensure!(self.pos == end, "array element runs past the length of the array");
                Ok(Value::Array(elem.to_string(), items))
            }
            b'(' => {
                self.align(8)?;
                let inner = &sig[1..sig.len() - 1];
                let mut items = Vec::new();
                for t in split_signature(inner)? {
                    items.push(self.value(t, depth + 1)?);
                }
                Ok(Value::Struct(items))
            }
            b'{' => {
                self.align(8)?;
                let inner = &sig[1..sig.len() - 1];
                let parts = split_signature(inner)?;
                let k = self.value(parts[0], depth + 1)?;
                let v = self.value(parts[1], depth + 1)?;
                Ok(Value::Entry(Box::new(k), Box::new(v)))
            }
            c => bail!("unknown type {:?}", c as char),
        }
    }
}

// ---- messages ----

pub const CALL: u8 = 1;
pub const RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub signature: String,
    pub unix_fds: u32,
    pub body: Vec<Value>,
}

/// Valid interface, error and well-known bus names share one syntax: elements of
/// `[A-Za-z_][A-Za-z0-9_]*` separated by `.`, at least two; a bus name may also have `-` in an
/// element. `bus` allows the `-`. At most 255 bytes.
pub fn valid_dotted_name(s: &str, bus: bool) -> bool {
    if s.is_empty() || s.len() > 255 {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|e| {
            let mut c = e.bytes();
            matches!(c.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_' || (bus && b == b'-')) && c.all(|b| b.is_ascii_alphanumeric() || b == b'_' || (bus && b == b'-'))
        })
}

fn valid_member(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && !s.as_bytes()[0].is_ascii_digit() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Total length of the message whose fixed header is in `head` (the first 16 bytes), checked
/// against the limits. Call it before reading or allocating the rest.
pub fn message_len(head: &[u8; 16], lim: &Limits) -> Result<usize> {
    let big = match head[0] {
        b'l' => false,
        b'B' => true,
        c => bail!("unknown byte order {c:#x}"),
    };
    ensure!(head[3] == 1, "protocol version {} is not supported", head[3]);
    let rd = |o: usize| -> u32 {
        let b: [u8; 4] = head[o..o + 4].try_into().unwrap();
        if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    };
    let (body, fields) = (rd(4) as usize, rd(12) as usize);
    ensure!(fields <= lim.max_header_fields, "header fields longer than the limit ({fields} bytes)");
    let total = 16usize.checked_add(fields.div_ceil(8) * 8).and_then(|t| t.checked_add(body));
    match total {
        Some(t) if t <= lim.max_message => Ok(t),
        _ => bail!("message longer than the limit"),
    }
}

/// Parse a whole message. `avail_fds` is how many descriptors are waiting; the message says how
/// many of them are its own (`unix_fds`), and it is refused if it claims more than are waiting.
pub fn parse_message(buf: &[u8], avail_fds: usize, lim: &Limits) -> Result<Message> {
    ensure!(buf.len() >= 16, "message shorter than its fixed header");
    let head: [u8; 16] = buf[..16].try_into().unwrap();
    let total = message_len(&head, lim)?;
    ensure!(buf.len() == total, "message is {} bytes, header says {total}", buf.len());
    let big = head[0] == b'B';
    let mut m = Message { kind: head[1], flags: head[2], ..Default::default() };
    ensure!((CALL..=SIGNAL).contains(&m.kind), "unknown message type {}", m.kind);
    let rd32 = |o: usize| -> u32 {
        let b: [u8; 4] = head[o..o + 4].try_into().unwrap();
        if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    };
    m.serial = rd32(8);
    ensure!(m.serial != 0, "serial 0");
    let body_len = rd32(4) as usize;
    let fields_len = rd32(12) as usize;
    let mut r = Reader::new(&buf[..16 + fields_len], 12, big, 0, *lim);
    let fields = r.value("a(yv)", 0)?;
    // The header ends at a multiple of 8; the bytes between are zero padding.
    ensure!(buf[16 + fields_len..16 + fields_len.div_ceil(8) * 8].iter().all(|b| *b == 0), "padding is not zero");
    let Value::Array(_, items) = fields else { bail!("header fields") };
    let mut seen = [false; 10];
    for it in items {
        let Value::Struct(mut p) = it else { bail!("header field") };
        let (Some(Value::Variant(val)), Some(Value::Byte(code))) = (p.pop(), p.pop()) else { bail!("header field") };
        let code = code as usize;
        if code == 0 || code > 9 {
            continue; // unknown fields are ignored, as the protocol says
        }
        ensure!(!seen[code], "header field {code} twice");
        seen[code] = true;
        match (code, *val) {
            (1, Value::Path(s)) => m.path = Some(s),
            (2, Value::Str(s)) => {
                ensure!(valid_dotted_name(&s, false), "invalid interface name");
                m.interface = Some(s)
            }
            (3, Value::Str(s)) => {
                ensure!(valid_member(&s), "invalid member name");
                m.member = Some(s)
            }
            (4, Value::Str(s)) => {
                ensure!(valid_dotted_name(&s, false), "invalid error name");
                m.error_name = Some(s)
            }
            (5, Value::U32(n)) => m.reply_serial = Some(n),
            (6, Value::Str(s)) => {
                ensure!(s.starts_with(':') || valid_dotted_name(&s, true), "invalid destination");
                m.destination = Some(s)
            }
            (7, Value::Str(s)) => {
                ensure!(s.starts_with(':') || valid_dotted_name(&s, true), "invalid sender");
                m.sender = Some(s)
            }
            (8, Value::Sig(s)) => m.signature = s,
            (9, Value::U32(n)) => m.unix_fds = n,
            _ => bail!("header field {code} has the wrong type"),
        }
    }
    match m.kind {
        CALL => ensure!(m.path.is_some() && m.member.is_some(), "method call without path or member"),
        SIGNAL => ensure!(m.path.is_some() && m.interface.is_some() && m.member.is_some(), "signal without path, interface or member"),
        ERROR => ensure!(m.error_name.is_some() && m.reply_serial.is_some(), "error without name or reply serial"),
        _ => ensure!(m.reply_serial.is_some(), "method return without reply serial"),
    }
    ensure!(m.unix_fds as usize <= lim.max_fds, "message announces {} descriptors, limit is {}", m.unix_fds, lim.max_fds);
    ensure!(m.unix_fds as usize <= avail_fds, "message announces {} descriptors, {avail_fds} arrived", m.unix_fds);
    let nfds = m.unix_fds as usize;
    let body_start = 16 + fields_len.div_ceil(8) * 8;
    let mut r = Reader::new(&buf[..body_start + body_len], body_start, big, nfds, *lim);
    for t in split_signature(&m.signature)? {
        m.body.push(r.value(t, 0)?);
    }
    ensure!(r.pos() == body_start + body_len, "bytes left over after the body");
    ensure!(m.signature.is_empty() == (body_len == 0), "body and signature disagree");
    Ok(m)
}

/// Encode a message (little endian). The header fields are only those the caller sets.
pub fn encode_message(m: &Message) -> Result<Vec<u8>> {
    check_signature(&m.signature)?;
    let mut body = Writer::new();
    // The body is aligned as if it started at a multiple of 8, which it does.
    let sigs = split_signature(&m.signature)?;
    ensure!(sigs.len() == m.body.len(), "body has {} values, signature has {}", m.body.len(), sigs.len());
    for (v, s) in m.body.iter().zip(&sigs) {
        ensure!(v.signature() == *s, "body value has signature {} instead of {s}", v.signature());
        body.value(v)?;
    }
    let mut fields = Vec::new();
    let mut field = |code: u8, v: Value| fields.push(Value::Struct(vec![Value::Byte(code), Value::variant(v)]));
    if let Some(s) = &m.path {
        field(1, Value::Path(s.clone()));
    }
    if let Some(s) = &m.interface {
        field(2, Value::Str(s.clone()));
    }
    if let Some(s) = &m.member {
        field(3, Value::Str(s.clone()));
    }
    if let Some(s) = &m.error_name {
        field(4, Value::Str(s.clone()));
    }
    if let Some(n) = m.reply_serial {
        field(5, Value::U32(n));
    }
    if let Some(s) = &m.destination {
        field(6, Value::Str(s.clone()));
    }
    if !m.signature.is_empty() {
        field(8, Value::Sig(m.signature.clone()));
    }
    if m.unix_fds > 0 {
        field(9, Value::U32(m.unix_fds));
    }
    let mut w = Writer::new();
    w.buf.extend_from_slice(&[b'l', m.kind, m.flags, 1]);
    w.buf.extend_from_slice(&(body.buf.len() as u32).to_le_bytes());
    w.buf.extend_from_slice(&m.serial.to_le_bytes());
    w.value(&Value::Array("(yv)".into(), fields))?;
    w.pad(8);
    w.buf.extend_from_slice(&body.buf);
    Ok(w.buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: Value) {
        let mut w = Writer::new();
        w.value(&v).unwrap();
        let sig = v.signature();
        let mut r = Reader::new(&w.buf, 0, false, 4, Limits::default());
        assert_eq!(r.value(&sig, 0).unwrap(), v, "{sig}");
        assert_eq!(r.pos(), w.buf.len());
    }

    #[test]
    fn values_survive_a_round_trip() {
        rt(Value::Byte(7));
        rt(Value::Bool(true));
        rt(Value::I16(-2));
        rt(Value::U32(0xdead_beef));
        rt(Value::I64(-5));
        rt(Value::F64(1.5));
        rt(Value::str("héllo"));
        rt(Value::Path("/org/freedesktop/DBus".into()));
        rt(Value::Sig("a{sv}".into()));
        rt(Value::Fd(3));
        rt(Value::Array("s".into(), vec![Value::str("a"), Value::str("bc")]));
        rt(Value::Array("u".into(), vec![]));
        rt(Value::Array("(sv)".into(), vec![Value::Struct(vec![Value::str("PIDs"), Value::variant(Value::Array("u".into(), vec![Value::U32(1)]))])]));
        rt(Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::str("k")), Box::new(Value::variant(Value::U64(9))))]));
        rt(Value::Array("x".into(), vec![Value::I64(1), Value::I64(2)]));
    }

    #[test]
    fn signatures_are_checked() {
        for ok in ["", "s", "a{sv}", "a(ss)", "(i(ii))", "aay", "ssa{sv}as", "ah"] {
            check_signature(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in ["a", "(", "()", "(i", "i)", "{sv}", "a{vs}", "a{s}", "a{svx}", "z", "a{(i)s}", "aa", "(a)"] {
            assert!(check_signature(bad).is_err(), "{bad} was accepted");
        }
        assert!(check_signature(&"i".repeat(256)).is_err());
        assert!(check_signature(&format!("{}i", "a".repeat(33))).is_err());
        assert!(check_signature(&format!("{}i{}", "(".repeat(33), ")".repeat(33))).is_err());
        assert!(check_single("ii").is_err());
        check_single("a(ii)").unwrap();
    }

    #[test]
    fn object_paths_and_names() {
        assert!(valid_object_path("/"));
        assert!(valid_object_path("/a/b_1"));
        for bad in ["", "a", "//", "/a/", "/a//b", "/a-b", "/é"] {
            assert!(!valid_object_path(bad), "{bad}");
        }
        assert!(valid_dotted_name("org.example.App", false));
        assert!(!valid_dotted_name("org", false));
        assert!(!valid_dotted_name("org.1x", false));
        assert!(!valid_dotted_name("org.a-b", false));
        assert!(valid_dotted_name("org.a-b", true));
        assert!(!valid_dotted_name(&format!("a.{}", "b".repeat(300)), false));
    }

    fn call() -> Message {
        Message {
            kind: CALL,
            serial: 5,
            path: Some("/org/x".into()),
            interface: Some("org.x.I".into()),
            member: Some("Do".into()),
            destination: Some("org.x".into()),
            signature: "sa{sv}h".into(),
            unix_fds: 1,
            body: vec![Value::str("hi"), Value::Array("{sv}".into(), vec![Value::Entry(Box::new(Value::str("n")), Box::new(Value::variant(Value::U32(3))))]), Value::Fd(0)],
            ..Default::default()
        }
    }

    #[test]
    fn a_message_survives_a_round_trip() {
        let m = call();
        let b = encode_message(&m).unwrap();
        let back = parse_message(&b, 1, &Limits::default()).unwrap();
        assert_eq!(back, m);
        // A message that claims a descriptor that did not arrive is refused.
        assert!(parse_message(&b, 0, &Limits::default()).is_err());
    }

    #[test]
    fn big_endian_messages_are_read() {
        // method call "Ping" with no body, big endian, hand-assembled.
        let mut b = Vec::new();
        b.extend_from_slice(&[b'B', 1, 0, 1]);
        b.extend_from_slice(&0u32.to_be_bytes()); // body
        b.extend_from_slice(&1u32.to_be_bytes()); // serial
        let mut f = Vec::new();
        // PATH "/"
        f.extend_from_slice(&[1, 1, b'o', 0]);
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(b"/\0");
        while f.len() % 8 != 0 {
            f.push(0);
        }
        // MEMBER "Ping"
        f.extend_from_slice(&[3, 1, b's', 0]);
        f.extend_from_slice(&4u32.to_be_bytes());
        f.extend_from_slice(b"Ping\0");
        b.extend_from_slice(&(f.len() as u32).to_be_bytes());
        b.extend_from_slice(&f);
        while b.len() % 8 != 0 {
            b.push(0);
        }
        let m = parse_message(&b, 0, &Limits::default()).unwrap();
        assert_eq!((m.member.as_deref(), m.path.as_deref(), m.serial), (Some("Ping"), Some("/"), 1));
    }

    #[test]
    fn limits_are_enforced_before_allocating() {
        let mut head = [0u8; 16];
        head[0] = b'l';
        head[1] = 1;
        head[3] = 1;
        head[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(message_len(&head, &Limits::default()).is_err());
        head[4..8].copy_from_slice(&0u32.to_le_bytes());
        head[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(message_len(&head, &Limits::default()).is_err());
        head[12..16].copy_from_slice(&8u32.to_le_bytes());
        head[3] = 2;
        assert!(message_len(&head, &Limits::default()).is_err(), "version 2");
        head[3] = 1;
        head[0] = b'x';
        assert!(message_len(&head, &Limits::default()).is_err(), "byte order");
        // An array longer than the limit, a string longer than the limit.
        let lim = Limits { max_string: 4, max_array_elems: 2, ..Limits::default() };
        let mut w = Writer::new();
        w.value(&Value::str("hello")).unwrap();
        assert!(Reader::new(&w.buf, 0, false, 0, lim).value("s", 0).is_err());
        let mut w = Writer::new();
        w.value(&Value::Array("y".into(), vec![Value::Byte(1), Value::Byte(2), Value::Byte(3)])).unwrap();
        assert!(Reader::new(&w.buf, 0, false, 0, lim).value("ay", 0).is_err());
    }

    #[test]
    fn nesting_is_bounded() {
        // 40 variants inside each other.
        let mut b = Vec::new();
        for _ in 0..40 {
            b.extend_from_slice(&[1, b'v', 0]);
        }
        b.extend_from_slice(&[1, b'y', 0, 9]);
        assert!(Reader::new(&b, 0, false, 0, Limits::default()).value("v", 0).is_err());
    }

    #[test]
    fn malformed_values_are_refused() {
        let l = Limits::default();
        let rd = |b: &[u8], s: &str| Reader::new(b, 0, false, 2, l).value(s, 0);
        assert!(rd(&[2, 0, 0, 0], "b").is_err(), "boolean 2");
        assert!(rd(&[5, 0, 0, 0, b'a', b'b', b'c', b'd', b'e', 1], "s").is_err(), "no NUL");
        assert!(rd(&[1, 0, 0, 0, 0, 0], "s").is_err(), "NUL inside");
        assert!(rd(&[2, 0, 0, 0, 0xff, 0xfe, 0], "s").is_err(), "not UTF-8");
        assert!(rd(&[1, 0, 0, 0], "h").is_ok());
        assert!(rd(&[2, 0, 0, 0], "h").is_err(), "descriptor index past the ones that arrived");
        assert!(rd(&[7, 0, 0, 0], "h").is_err());
        // padding that is not zero
        let mut r = Reader::new(&[1, 9, 0, 0, 5, 0, 0, 0], 0, false, 0, l);
        r.value("y", 0).unwrap();
        assert!(r.value("u", 0).is_err(), "non-zero padding");
        // an array whose elements run past its length
        let mut w = Writer::new();
        w.value(&Value::Array("u".into(), vec![Value::U32(1), Value::U32(2)])).unwrap();
        w.buf[0] = 6; // length 6 is not a multiple of 4
        assert!(rd(&w.buf, "au").is_err());
    }

    /// The seeds of the fuzz target are real messages: the reader must accept every one.
    #[test]
    fn the_fuzz_seeds_are_valid_messages() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz/corpus/fuzz_dbus");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let b = std::fs::read(&p).unwrap();
            let avail = (b[0] % 20) as usize;
            parse_message(&b[1..], avail, &Limits::default()).unwrap_or_else(|e| panic!("{}: {e:#}", p.display()));
            n += 1;
        }
        assert!(n >= 6, "{n} seeds");
    }

    /// No input makes the reader panic: random bytes, and every prefix and single-byte change of
    /// a valid message.
    #[test]
    fn random_and_mutated_input_never_panics() {
        let l = Limits::default();
        let good = encode_message(&call()).unwrap();
        for n in 0..good.len() {
            let _ = parse_message(&good[..n], 1, &l);
        }
        for i in 0..good.len() {
            for v in [0u8, 1, 0x7f, 0x80, 0xff, good[i].wrapping_add(1)] {
                let mut m = good.clone();
                m[i] = v;
                let _ = parse_message(&m, 1, &l);
            }
        }
        // a small xorshift for the random part
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..20000 {
            let len = (next() % 200) as usize;
            let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if len >= 16 {
                b[0] = b'l';
                b[3] = 1;
            }
            let _ = parse_message(&b, 3, &l);
            for sig in ["a(yv)", "a{sv}", "(sa{sv})", "vvv", "aay", "a(s(ai))"] {
                let _ = Reader::new(&b, 0, next() % 2 == 0, 3, l).value(sig, 0);
            }
        }
    }
}
