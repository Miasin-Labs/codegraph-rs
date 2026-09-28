//! A reader (and a minimal writer) for the SCIP index format.
//!
//! SCIP (<https://github.com/sourcegraph/scip>) is a protobuf message; only
//! the fields the compiler layer uses are decoded, and the wire format is
//! walked by hand (varints and length-delimited fields — a few dozen lines)
//! rather than pulling in a protobuf code generator for five messages.
//! Unknown fields are skipped, so a newer SCIP producer still reads.
//!
//! Symbols are interned: every occurrence holds a [`SymbolId`] into
//! [`ScipIndex::symbols`], and every `local N` symbol is folded into the one
//! [`LOCAL`] id (the compiler layer only needs to know that a name is local,
//! never which local it is).
//!
//! The writer ([`encode`]) exists for the checked-in test fixtures and to
//! re-encode a filtered index; it writes the same field numbers back.

use std::collections::HashMap;

/// Index into [`ScipIndex::symbols`].
pub type SymbolId = u32;

/// The id every `local N` symbol maps to.
pub const LOCAL: SymbolId = 0;

/// `SymbolRole.Definition`.
pub const ROLE_DEFINITION: i32 = 1;

/// SCIP `SymbolInformation.Kind` values rust-analyzer emits (the ones this
/// crate reads; the rest stay plain integers).
pub mod kind {
    pub const ASSOCIATED_TYPE: i32 = 3;
    pub const CONSTANT: i32 = 8;
    pub const ENUM: i32 = 11;
    pub const ENUM_MEMBER: i32 = 12;
    pub const FIELD: i32 = 15;
    pub const FUNCTION: i32 = 17;
    pub const MACRO: i32 = 25;
    pub const METHOD: i32 = 26;
    pub const MODULE: i32 = 29;
    pub const STRUCT: i32 = 49;
    pub const TRAIT: i32 = 53;
    /// rust-analyzer files `impl` blocks (and type aliases) under this kind.
    pub const TYPE_ALIAS: i32 = 55;
    pub const UNION: i32 = 60;
    pub const TRAIT_METHOD: i32 = 70;
    pub const STATIC_METHOD: i32 = 80;
    pub const STATIC_VARIABLE: i32 = 82;
}

/// A zero-based source range. `start_col`/`end_col` are UTF-8 byte offsets
/// from the line start (rust-analyzer declares `UTF8CodeUnitOffsetFromLineStart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Range {
    /// SCIP packs a range as `[line, start, end]` (single line) or
    /// `[start_line, start_col, end_line, end_col]`.
    fn from_packed(values: &[i32]) -> Option<Range> {
        let value = |i: usize| u32::try_from(*values.get(i)?).ok();
        match values.len() {
            3 => Some(Range {
                start_line: value(0)?,
                start_col: value(1)?,
                end_line: value(0)?,
                end_col: value(2)?,
            }),
            4 => Some(Range {
                start_line: value(0)?,
                start_col: value(1)?,
                end_line: value(2)?,
                end_col: value(3)?,
            }),
            _ => None,
        }
    }

    fn packed(&self) -> Vec<i32> {
        if self.start_line == self.end_line {
            vec![
                self.start_line as i32,
                self.start_col as i32,
                self.end_col as i32,
            ]
        } else {
            vec![
                self.start_line as i32,
                self.start_col as i32,
                self.end_line as i32,
                self.end_col as i32,
            ]
        }
    }

    /// Whether `(line, col)` (zero-based) lies inside the range.
    pub fn contains(&self, line: u32, col: u32) -> bool {
        (line, col) >= (self.start_line, self.start_col)
            && (line, col) < (self.end_line, self.end_col)
    }
}

/// One occurrence of a symbol in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub range: Range,
    pub symbol: SymbolId,
    pub roles: i32,
    /// The whole syntax node the occurrence names (a definition's item).
    pub enclosing: Option<Range>,
}

impl Occurrence {
    pub fn is_definition(&self) -> bool {
        self.roles & ROLE_DEFINITION != 0
    }
}

/// What the producer says about one symbol it defines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolInfo {
    pub symbol: SymbolId,
    pub kind: i32,
    pub display_name: String,
    /// The signature as rust-analyzer renders it (`pub fn f() -> u32`).
    pub signature: Option<String>,
    pub documentation: Vec<String>,
}

/// One source file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub relative_path: String,
    pub language: String,
    pub occurrences: Vec<Occurrence>,
    pub symbols: Vec<SymbolInfo>,
    pub position_encoding: i32,
}

/// A decoded SCIP index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipIndex {
    pub tool_name: String,
    pub tool_version: String,
    pub project_root: String,
    pub documents: Vec<Document>,
    /// Interned symbol strings; `symbols[LOCAL]` is `"local"`.
    pub symbols: Vec<String>,
}

impl Default for ScipIndex {
    fn default() -> Self {
        ScipIndex {
            tool_name: String::new(),
            tool_version: String::new(),
            project_root: String::new(),
            documents: Vec::new(),
            symbols: vec!["local".to_string()],
        }
    }
}

impl ScipIndex {
    pub fn symbol(&self, id: SymbolId) -> &str {
        self.symbols.get(id as usize).map_or("", String::as_str)
    }

    /// Decode a SCIP index. Fails only on malformed wire data.
    pub fn decode(bytes: &[u8]) -> Result<ScipIndex, DecodeError> {
        let mut index = ScipIndex::default();
        let mut interner = Interner::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, value)) = reader.field()? {
            match (field, value) {
                (1, Value::Bytes(metadata)) => index.read_metadata(metadata)?,
                (2, Value::Bytes(document)) => {
                    let document = read_document(document, &mut interner)?;
                    index.documents.push(document);
                }
                _ => {}
            }
        }
        index.symbols = interner.into_symbols();
        Ok(index)
    }

    fn read_metadata(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        let mut reader = Reader::new(bytes);
        while let Some((field, value)) = reader.field()? {
            match (field, value) {
                (2, Value::Bytes(tool)) => {
                    let mut tool = Reader::new(tool);
                    while let Some((field, value)) = tool.field()? {
                        match (field, value) {
                            (1, Value::Bytes(name)) => self.tool_name = text(name)?,
                            (2, Value::Bytes(version)) => self.tool_version = text(version)?,
                            _ => {}
                        }
                    }
                }
                (3, Value::Bytes(root)) => self.project_root = text(root)?,
                _ => {}
            }
        }
        Ok(())
    }
}

fn read_document(bytes: &[u8], interner: &mut Interner) -> Result<Document, DecodeError> {
    let mut document = Document::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.field()? {
        match (field, value) {
            (1, Value::Bytes(path)) => document.relative_path = text(path)?,
            (2, Value::Bytes(occurrence)) => {
                if let Some(occurrence) = read_occurrence(occurrence, interner)? {
                    document.occurrences.push(occurrence);
                }
            }
            (3, Value::Bytes(symbol)) => document.symbols.push(read_symbol(symbol, interner)?),
            (4, Value::Bytes(language)) => document.language = text(language)?,
            (6, Value::Varint(encoding)) => document.position_encoding = encoding as i32,
            _ => {}
        }
    }
    Ok(document)
}

fn read_occurrence(
    bytes: &[u8],
    interner: &mut Interner,
) -> Result<Option<Occurrence>, DecodeError> {
    let mut range = Vec::new();
    let mut enclosing = Vec::new();
    let mut symbol = None;
    let mut roles = 0;
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.field()? {
        match (field, value) {
            (1, value) => push_int32s(value, &mut range)?,
            (2, Value::Bytes(name)) => symbol = Some(interner.intern(name)?),
            (3, Value::Varint(value)) => roles = value as i32,
            (7, value) => push_int32s(value, &mut enclosing)?,
            _ => {}
        }
    }
    // An occurrence without a symbol or a readable range says nothing.
    let (Some(symbol), Some(range)) = (symbol, Range::from_packed(&range)) else {
        return Ok(None);
    };
    Ok(Some(Occurrence {
        range,
        symbol,
        roles,
        enclosing: Range::from_packed(&enclosing),
    }))
}

fn read_symbol(bytes: &[u8], interner: &mut Interner) -> Result<SymbolInfo, DecodeError> {
    let mut info = SymbolInfo::default();
    let mut reader = Reader::new(bytes);
    while let Some((field, value)) = reader.field()? {
        match (field, value) {
            (1, Value::Bytes(name)) => info.symbol = interner.intern(name)?,
            (3, Value::Bytes(doc)) => info.documentation.push(text(doc)?),
            (5, Value::Varint(kind)) => info.kind = kind as i32,
            (6, Value::Bytes(name)) => info.display_name = text(name)?,
            (7, Value::Bytes(signature)) => {
                // `signature_documentation` is a Document whose `text` (5)
                // holds the rendered signature.
                let mut signature_reader = Reader::new(signature);
                while let Some((field, value)) = signature_reader.field()? {
                    if let (5, Value::Bytes(rendered)) = (field, value) {
                        info.signature = Some(text(rendered)?);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(info)
}

/// Append an `int32` field's values: packed (length-delimited) or one varint.
fn push_int32s(value: Value<'_>, out: &mut Vec<i32>) -> Result<(), DecodeError> {
    match value {
        Value::Varint(value) => out.push(value as i32),
        Value::Bytes(packed) => {
            let mut reader = Reader::new(packed);
            while !reader.at_end() {
                out.push(reader.varint()? as i32);
            }
        }
        Value::Fixed => {}
    }
    Ok(())
}

fn text(bytes: &[u8]) -> Result<String, DecodeError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError("invalid UTF-8 in a string field"))
}

/// Why an index could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError(pub &'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed SCIP index: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

#[derive(Default)]
struct Interner {
    ids: HashMap<String, SymbolId>,
    symbols: Vec<String>,
}

impl Interner {
    fn intern(&mut self, bytes: &[u8]) -> Result<SymbolId, DecodeError> {
        if bytes.starts_with(b"local ") {
            return Ok(LOCAL);
        }
        let name = std::str::from_utf8(bytes).map_err(|_| DecodeError("invalid UTF-8 symbol"))?;
        if let Some(id) = self.ids.get(name) {
            return Ok(*id);
        }
        // Id 0 is LOCAL; real symbols start at 1.
        let id = self.symbols.len() as SymbolId + 1;
        self.ids.insert(name.to_string(), id);
        self.symbols.push(name.to_string());
        Ok(id)
    }

    fn into_symbols(self) -> Vec<String> {
        let mut symbols = Vec::with_capacity(self.symbols.len() + 1);
        symbols.push("local".to_string());
        symbols.extend(self.symbols);
        symbols
    }
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// A fixed32/fixed64 field (none are read).
    Fixed,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn at_end(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .bytes
                .get(self.at)
                .ok_or(DecodeError("truncated varint"))?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(DecodeError("varint too long"))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(DecodeError("truncated field"))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    /// The next field, or `None` at the end of the message.
    fn field(&mut self) -> Result<Option<(u64, Value<'a>)>, DecodeError> {
        if self.at_end() {
            return Ok(None);
        }
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Fixed
            }
            2 => {
                let len = usize::try_from(self.varint()?)
                    .map_err(|_| DecodeError("field length overflows"))?;
                Value::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Value::Fixed
            }
            _ => return Err(DecodeError("unsupported wire type")),
        };
        Ok(Some((key >> 3, value)))
    }
}

// =============================================================================
// Writer
// =============================================================================

/// Encode `index` back into SCIP wire format (the fields [`ScipIndex::decode`]
/// reads). Local symbols are written as `local 0`.
pub fn encode(index: &ScipIndex) -> Vec<u8> {
    let mut out = Vec::new();
    let mut metadata = Vec::new();
    let mut tool = Vec::new();
    put_bytes(&mut tool, 1, index.tool_name.as_bytes());
    put_bytes(&mut tool, 2, index.tool_version.as_bytes());
    put_bytes(&mut metadata, 2, &tool);
    put_bytes(&mut metadata, 3, index.project_root.as_bytes());
    put_bytes(&mut out, 1, &metadata);
    let symbol = |id: SymbolId| -> &str {
        if id == LOCAL {
            "local 0"
        } else {
            index.symbol(id)
        }
    };
    for document in &index.documents {
        let mut doc = Vec::new();
        put_bytes(&mut doc, 4, document.language.as_bytes());
        put_bytes(&mut doc, 1, document.relative_path.as_bytes());
        for occurrence in &document.occurrences {
            let mut occ = Vec::new();
            put_packed(&mut occ, 1, &occurrence.range.packed());
            put_bytes(&mut occ, 2, symbol(occurrence.symbol).as_bytes());
            if occurrence.roles != 0 {
                put_varint_field(&mut occ, 3, occurrence.roles as u64);
            }
            if let Some(enclosing) = occurrence.enclosing {
                put_packed(&mut occ, 7, &enclosing.packed());
            }
            put_bytes(&mut doc, 2, &occ);
        }
        for info in &document.symbols {
            let mut sym = Vec::new();
            put_bytes(&mut sym, 1, symbol(info.symbol).as_bytes());
            for line in &info.documentation {
                put_bytes(&mut sym, 3, line.as_bytes());
            }
            if info.kind != 0 {
                put_varint_field(&mut sym, 5, info.kind as u64);
            }
            if !info.display_name.is_empty() {
                put_bytes(&mut sym, 6, info.display_name.as_bytes());
            }
            if let Some(signature) = &info.signature {
                let mut sig = Vec::new();
                put_bytes(&mut sig, 4, b"rust");
                put_bytes(&mut sig, 5, signature.as_bytes());
                put_bytes(&mut sym, 7, &sig);
            }
            put_bytes(&mut doc, 3, &sym);
        }
        if document.position_encoding != 0 {
            put_varint_field(&mut doc, 6, document.position_encoding as u64);
        }
        put_bytes(&mut out, 2, &doc);
    }
    out
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    put_varint(out, field << 3);
    put_varint(out, value);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_packed(out: &mut Vec<u8>, field: u64, values: &[i32]) {
    let mut packed = Vec::new();
    for value in values {
        put_varint(&mut packed, *value as u32 as u64);
    }
    put_bytes(out, field, &packed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ScipIndex {
        ScipIndex {
            tool_name: "rust-analyzer".into(),
            tool_version: "test".into(),
            project_root: "file:///fixture".into(),
            documents: vec![Document {
                relative_path: "src/lib.rs".into(),
                language: "rust".into(),
                occurrences: vec![
                    Occurrence {
                        range: Range {
                            start_line: 0,
                            start_col: 7,
                            end_line: 0,
                            end_col: 13,
                        },
                        symbol: 1,
                        roles: ROLE_DEFINITION,
                        enclosing: Some(Range {
                            start_line: 0,
                            start_col: 0,
                            end_line: 2,
                            end_col: 1,
                        }),
                    },
                    Occurrence {
                        range: Range {
                            start_line: 1,
                            start_col: 4,
                            end_line: 1,
                            end_col: 5,
                        },
                        symbol: LOCAL,
                        roles: 0,
                        enclosing: None,
                    },
                ],
                symbols: vec![SymbolInfo {
                    symbol: 1,
                    kind: kind::FUNCTION,
                    display_name: "helper".into(),
                    signature: Some("pub fn helper() -> u32".into()),
                    documentation: vec!["Docs.".into()],
                }],
                position_encoding: 1,
            }],
            symbols: vec![
                "local".into(),
                "rust-analyzer cargo fx 0.1.0 helper().".into(),
            ],
        }
    }

    #[test]
    fn round_trips_through_the_wire_format() {
        let index = sample();
        let decoded = ScipIndex::decode(&encode(&index)).unwrap();
        assert_eq!(decoded, index);
    }

    #[test]
    fn locals_share_one_id_and_symbols_are_interned() {
        let mut index = sample();
        let doc = &mut index.documents[0];
        doc.occurrences.push(doc.occurrences[0].clone());
        let decoded = ScipIndex::decode(&encode(&index)).unwrap();
        assert_eq!(decoded.symbols.len(), 2);
        assert_eq!(decoded.documents[0].occurrences[1].symbol, LOCAL);
        assert_eq!(decoded.documents[0].occurrences[2].symbol, 1);
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let bytes = encode(&sample());
        for cut in [1, 5, bytes.len() / 2, bytes.len() - 1] {
            assert!(ScipIndex::decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let mut bytes = Vec::new();
        put_varint_field(&mut bytes, 9, 42);
        put_bytes(&mut bytes, 12, b"future");
        bytes.extend(encode(&sample()));
        assert_eq!(ScipIndex::decode(&bytes).unwrap(), sample());
    }

    #[test]
    fn ranges_unpack_both_shapes() {
        assert_eq!(
            Range::from_packed(&[3, 4, 9]),
            Some(Range {
                start_line: 3,
                start_col: 4,
                end_line: 3,
                end_col: 9
            })
        );
        assert_eq!(
            Range::from_packed(&[3, 4, 5, 1]),
            Some(Range {
                start_line: 3,
                start_col: 4,
                end_line: 5,
                end_col: 1
            })
        );
        assert_eq!(Range::from_packed(&[1, 2]), None);
        assert_eq!(Range::from_packed(&[-1, 2, 3]), None);
    }
}
