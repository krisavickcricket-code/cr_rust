//! High-performance XML-to-CSV extractor — Rust port of the VB.NET `CR` app.
//!
//! ## Performance design
//!
//! * **Custom zero-copy byte scanner** — no `quick-xml`, no `String`, no `Cow`,
//!   no entity resolution.  Every event name / attribute value / text content is
//!   a `&[u8]` slice borrowed directly from the in-memory file buffer.
//! * **Single disk read** — `fs::read` loads the file once; both the
//!   schema-discovery and data-extraction passes scan the same `&[u8]`.
//! * **Pre-opened `BufWriter`s** — every output CSV is opened once with a 256 KB
//!   buffer before the extraction loop.  Zero file-open/close per record.
//! * **Reusable buffers** — value slots, escape buffer, and row buffer are
//!   allocated once and cleared (not reallocated) per record.
//! * **Fast-path `clean_value`** — a single `contains(&b'#')` check skips the
//!   entire cleaning logic for the vast majority of values.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ────────────────────────────────────────────────────────────────────────────
//  Public types
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Progress {
    Log(String),
    Done,
    Error(String),
}

// ────────────────────────────────────────────────────────────────────────────
//  Zero-copy byte-level XML scanner
// ────────────────────────────────────────────────────────────────────────────

/// A scanned XML event.  All `&[u8]` slices borrow from the original data
/// buffer — zero allocation.
enum XmlEvent<'a> {
    /// `<name ...>`  (first_attr = first attribute's value, if any)
    Start {
        name: &'a [u8],
        first_attr: Option<&'a [u8]>,
    },
    /// `</name>`
    End,
    /// `<name .../>`
    Empty {
        name: &'a [u8],
        first_attr: Option<&'a [u8]>,
    },
    /// Text content between tags (whitespace-only text is already skipped).
    Text(&'a [u8]),
    Eof,
}

struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    #[inline]
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Advance to the next XML event, skipping comments, PIs, and
    /// whitespace-only text.
    fn next(&mut self) -> XmlEvent<'a> {
        loop {
            if self.pos >= self.data.len() {
                return XmlEvent::Eof;
            }

            // ── Text content until next '<' ──────────────────────────────
            if self.data[self.pos] != b'<' {
                let start = self.pos;
                // Find next '<'
                self.pos += 1;
                while self.pos < self.data.len() && self.data[self.pos] != b'<' {
                    self.pos += 1;
                }
                let text = &self.data[start..self.pos];
                // Skip whitespace-only text
                if text.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
                    continue;
                }
                return XmlEvent::Text(text);
            }

            // ── We're at '<' ──────────────────────────────────────────────
            self.pos += 1; // skip '<'
            if self.pos >= self.data.len() {
                return XmlEvent::Eof;
            }

            match self.data[self.pos] {
                // End tag: </name>
                b'/' => {
                    self.pos += 1;
                    // Skip to '>'
                    while self.pos < self.data.len() && self.data[self.pos] != b'>' {
                        self.pos += 1;
                    }
                    if self.pos < self.data.len() {
                        self.pos += 1;
                    }
                    return XmlEvent::End;
                }

                // Comment, CDATA, or DOCTYPE
                b'!' => {
                    self.pos += 1;
                    // CDATA: <![CDATA[...]]>
                    if self.pos + 7 <= self.data.len()
                        && &self.data[self.pos..self.pos + 7] == b"[CDATA["
                    {
                        self.pos += 7;
                        let start = self.pos;
                        // Find ]]>
                        while self.pos + 3 <= self.data.len() {
                            if self.data[self.pos] == b']'
                                && self.data[self.pos + 1] == b']'
                                && self.data[self.pos + 2] == b'>'
                            {
                                break;
                            }
                            self.pos += 1;
                        }
                        let text = &self.data[start..self.pos];
                        self.pos += 3; // skip ]]>
                        return XmlEvent::Text(text);
                    }
                    // Comment: <!-- ... -->
                    if self.pos + 1 < self.data.len()
                        && self.data[self.pos] == b'-'
                        && self.data[self.pos + 1] == b'-'
                    {
                        self.pos += 2;
                        // Find -->
                        while self.pos + 3 <= self.data.len() {
                            if self.data[self.pos] == b'-'
                                && self.data[self.pos + 1] == b'-'
                                && self.data[self.pos + 2] == b'>'
                            {
                                self.pos += 3;
                                break;
                            }
                            self.pos += 1;
                        }
                        continue;
                    }
                    // DOCTYPE or other: skip to '>'
                    while self.pos < self.data.len() && self.data[self.pos] != b'>' {
                        self.pos += 1;
                    }
                    if self.pos < self.data.len() {
                        self.pos += 1;
                    }
                    continue;
                }

                // Processing instruction: <?...?>
                b'?' => {
                    self.pos += 1;
                    while self.pos < self.data.len() && self.data[self.pos] != b'>' {
                        self.pos += 1;
                    }
                    if self.pos < self.data.len() {
                        self.pos += 1;
                    }
                    continue;
                }

                // Start tag or self-closing tag
                _ => {
                    // Read element name
                    let name_start = self.pos;
                    while self.pos < self.data.len()
                        && !is_name_delim(self.data[self.pos])
                    {
                        self.pos += 1;
                    }
                    let name = &self.data[name_start..self.pos];

                    // Skip whitespace
                    skip_ws(self.data, &mut self.pos);

                    if self.pos >= self.data.len() {
                        return XmlEvent::Start { name, first_attr: None };
                    }

                    // Self-closing with no attributes: <name/>
                    if self.data[self.pos] == b'/' {
                        self.pos += 1;
                        if self.pos < self.data.len() && self.data[self.pos] == b'>' {
                            self.pos += 1;
                        }
                        return XmlEvent::Empty { name, first_attr: None };
                    }

                    // Start with no attributes: <name>
                    if self.data[self.pos] == b'>' {
                        self.pos += 1;
                        return XmlEvent::Start { name, first_attr: None };
                    }

                    // Has attributes — read the first one
                    let first_attr = read_first_attr(self.data, &mut self.pos);

                    // Skip remaining attributes until '>' or '/>'
                    let is_self_closing = skip_to_end_tag(self.data, &mut self.pos);

                    if is_self_closing {
                        return XmlEvent::Empty { name, first_attr };
                    }

                    return XmlEvent::Start { name, first_attr };
                }
            }
        }
    }
}

#[inline]
fn is_name_delim(b: u8) -> bool {
    matches!(b, b' ' | b'>' | b'/' | b'\t' | b'\n' | b'\r')
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

#[inline]
fn skip_ws(data: &[u8], pos: &mut usize) {
    while *pos < data.len() && is_ws(data[*pos]) {
        *pos += 1;
    }
}

/// Read the first attribute's value from the current position.
/// Returns the value bytes (without quotes) and advances past all remaining
/// attributes up to `>` or `/>`.
fn read_first_attr<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    // Skip attribute name until '=' or whitespace
    while *pos < data.len() && data[*pos] != b'=' && !is_ws(data[*pos]) && data[*pos] != b'>' && data[*pos] != b'/' {
        *pos += 1;
    }
    skip_ws(data, pos);
    if *pos >= data.len() || data[*pos] != b'=' {
        return None;
    }
    *pos += 1; // skip '='
    skip_ws(data, pos);
    if *pos >= data.len() {
        return None;
    }
    let q = data[*pos];
    if q == b'"' || q == b'\'' {
        *pos += 1; // skip opening quote
        let val_start = *pos;
        while *pos < data.len() && data[*pos] != q {
            *pos += 1;
        }
        let val = &data[val_start..*pos];
        if *pos < data.len() {
            *pos += 1; // skip closing quote
        }
        return Some(val);
    }
    None
}

/// Skip remaining attributes until we hit `>` or `/>`, consuming it.
/// Returns `true` if the tag was self-closing (`/>`).
fn skip_to_end_tag(data: &[u8], pos: &mut usize) -> bool {
    while *pos < data.len() {
        match data[*pos] {
            b'>' => {
                *pos += 1;
                return false;
            }
            b'/' => {
                *pos += 1;
                if *pos < data.len() && data[*pos] == b'>' {
                    *pos += 1;
                }
                return true;
            }
            b'"' | b'\'' => {
                // Skip quoted attribute value
                let q = data[*pos];
                *pos += 1;
                while *pos < data.len() && data[*pos] != q {
                    *pos += 1;
                }
                if *pos < data.len() {
                    *pos += 1;
                }
            }
            _ => *pos += 1,
        }
    }
    false
}

// ────────────────────────────────────────────────────────────────────────────
//  Byte-level value helpers
// ────────────────────────────────────────────────────────────────────────────

/// Replace `#_{`→`{`, or `_{`→`{`, or strip `#` — byte-level, zero-alloc.
fn clean_value_into(input: &[u8], out: &mut Vec<u8>) {
    out.clear();
    // Fast path: 99%+ of CIM values contain neither '#' nor '_'.
    if !input.contains(&b'#') && !input.contains(&b'_') {
        out.extend_from_slice(input);
        return;
    }
    if find_bytes(input, b"#_{").is_some() {
        replace_all(input, b"#_{", b"{", out);
    } else if find_bytes(input, b"_{").is_some() {
        replace_all(input, b"_{", b"{", out);
    } else if input.contains(&b'#') {
        out.extend(input.iter().copied().filter(|&b| b != b'#'));
    } else {
        out.extend_from_slice(input);
    }
}

#[inline]
fn csv_escape_into(input: &[u8], out: &mut Vec<u8>) {
    out.clear();
    if input.contains(&b',') {
        out.extend(input.iter().map(|&b| if b == b',' { b';' } else { b }));
    } else {
        out.extend_from_slice(input);
    }
}

#[inline]
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn replace_all(input: &[u8], from: &[u8], to: &[u8], out: &mut Vec<u8>) {
    let fl = from.len();
    let mut i = 0;
    while i + fl <= input.len() {
        if &input[i..i + fl] == from {
            out.extend_from_slice(to);
            i += fl;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out.extend_from_slice(&input[i..]);
}

fn element_to_filename(name: &[u8]) -> Vec<u8> {
    name.iter().map(|&b| if b == b':' { b'-' } else { b }).collect()
}

// ────────────────────────────────────────────────────────────────────────────
//  Schema discovery (pass 1)
// ────────────────────────────────────────────────────────────────────────────

struct SchemaEntry {
    record_type: Vec<u8>,
    fields: Vec<Vec<u8>>, // [0]=record_type, [1..]=child element names
    field_set: HashSet<Vec<u8>>, // O(1) dedup lookup
}

fn discover_schema(data: &[u8]) -> Result<Vec<SchemaEntry>, String> {
    let mut sc = Scanner::new(data);

    let mut schema_list: Vec<SchemaEntry> = Vec::new();
    let mut schema_map: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut depth: i32 = 0;
    let mut cur_type: Option<Vec<u8>> = None;
    let mut cur_fields: Vec<Vec<u8>> = Vec::new();
    let mut cur_field_set: HashSet<Vec<u8>> = HashSet::new();

    loop {
        match sc.next() {
            XmlEvent::Start { name, .. } => {
                depth += 1;
                if depth == 2 {
                    cur_type = Some(name.to_vec());
                    cur_fields = vec![name.to_vec()];
                    cur_field_set.clear();
                    cur_field_set.insert(name.to_vec());
                } else if depth >= 3 {
                    // O(1) dedup via HashSet instead of O(N) linear scan
                    if cur_field_set.insert(name.to_vec()) {
                        cur_fields.push(name.to_vec());
                    }
                }
            }
            // Empty events don't change depth, so the depth checks are
            // one less than Start/End:  depth==1 → record, depth>=2 → field.
            XmlEvent::Empty { name, .. } => {
                if depth == 1 {
                    finish_schema_entry(name, vec![name.to_vec()], &mut schema_list, &mut schema_map);
                } else if depth >= 2 {
                    // O(1) dedup via HashSet instead of O(N) linear scan
                    if cur_field_set.insert(name.to_vec()) {
                        cur_fields.push(name.to_vec());
                    }
                }
            }
            XmlEvent::End => {
                if depth == 2 {
                    if let Some(rt) = cur_type.take() {
                        let fields = std::mem::take(&mut cur_fields);
                        finish_schema_entry(&rt, fields, &mut schema_list, &mut schema_map);
                    }
                }
                depth -= 1;
            }
            XmlEvent::Eof => break,
            XmlEvent::Text(_) => {} // ignore text in schema pass
        }
    }

    Ok(schema_list)
}

fn finish_schema_entry(
    record_type: &[u8],
    fields: Vec<Vec<u8>>,
    schema_list: &mut Vec<SchemaEntry>,
    schema_map: &mut HashMap<Vec<u8>, usize>,
) {
    if let Some(&idx) = schema_map.get(record_type) {
        // O(1) dedup via HashSet instead of O(N) linear scan
        for f in &fields {
            if !schema_list[idx].field_set.contains(f.as_slice()) {
                schema_list[idx].field_set.insert(f.clone());
                schema_list[idx].fields.push(f.clone());
            }
        }
    } else {
        let mut field_set = HashSet::new();
        for f in &fields {
            field_set.insert(f.clone());
        }
        let idx = schema_list.len();
        schema_map.insert(record_type.to_vec(), idx);
        schema_list.push(SchemaEntry {
            record_type: record_type.to_vec(),
            fields,
            field_set,
        });
    }
}

fn write_schema_csv(schema: &[SchemaEntry], path: &Path) -> Result<(), String> {
    let mut w = BufWriter::with_capacity(
        64 * 1024,
        fs::File::create(path).map_err(|e| format!("Create Schema List.csv: {e}"))?,
    );
    for entry in schema {
        let count = entry.fields.len() - 1;
        write!(w, "{count}").map_err(|e| format!("Write: {e}"))?;
        for f in &entry.fields {
            w.write_all(b",").map_err(|e| format!("Write: {e}"))?;
            w.write_all(f).map_err(|e| format!("Write: {e}"))?;
        }
        w.write_all(b"\n").map_err(|e| format!("Write: {e}"))?;
    }
    w.flush().map_err(|e| format!("Flush: {e}"))?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
//  Data extraction (pass 2)
// ────────────────────────────────────────────────────────────────────────────

fn extract_data(
    data: &[u8],
    out_folder: &Path,
    schema: &[SchemaEntry],
) -> Result<(), String> {

    // Pre-compute lookups (borrows into schema — zero clone)
    let schema_lookup: HashMap<&[u8], usize> = schema
        .iter()
        .enumerate()
        .map(|(i, e)| (e.record_type.as_slice(), i))
        .collect();
    let field_maps: Vec<HashMap<&[u8], usize>> = schema
        .iter()
        .map(|e| e.fields.iter().enumerate().map(|(i, f)| (f.as_slice(), i)).collect())
        .collect();

    // Pre-open every output CSV with a large BufWriter
    let mut writers: Vec<BufWriter<fs::File>> = Vec::with_capacity(schema.len());
    for entry in schema {
        let fname = element_to_filename(&entry.record_type);
        let fname_str = std::str::from_utf8(&fname).unwrap_or("unknown");
        let path: PathBuf = out_folder.join(format!("{fname_str}.CSV"));
        let mut w = BufWriter::with_capacity(
            256 * 1024,
            fs::File::create(&path).map_err(|e| format!("Create {}: {e}", path.display()))?,
        );
        // Header row
        for (i, f) in entry.fields.iter().enumerate() {
            if i > 0 {
                w.write_all(b",").map_err(|e| format!("Write: {e}"))?;
            }
            w.write_all(f).map_err(|e| format!("Write: {e}"))?;
        }
        w.write_all(b",\n").map_err(|e| format!("Write: {e}"))?;
        writers.push(w);
    }

    // Pre-allocate reusable buffers
    let max_fields = schema.iter().map(|e| e.fields.len()).max().unwrap_or(0);
    let mut value_slots: Vec<Vec<u8>> = (0..max_fields).map(|_| Vec::with_capacity(64)).collect();
    let mut value_set: Vec<bool> = vec![false; max_fields];
    let mut clean_buf: Vec<u8> = Vec::with_capacity(64);
    let mut escape_buf: Vec<u8> = Vec::with_capacity(64);
    let mut row_buf: Vec<u8> = Vec::with_capacity(4096);

    // Parse
    let mut sc = Scanner::new(data);
    let mut depth: i32 = 0;
    let mut cur_schema: Option<usize> = None;
    let mut pending_col: Option<usize> = None;

    loop {
        match sc.next() {
            XmlEvent::Start { name, first_attr } => {
                // If we expected text but got another element, previous field is empty
                if let Some(col) = pending_col.take() {
                    if cur_schema.is_some() {
                        value_slots[col].clear();
                        value_set[col] = true;
                    }
                }

                depth += 1;

                if depth == 2 {
                    // New record
                    if let Some(&si) = schema_lookup.get(name) {
                        cur_schema = Some(si);
                        let nf = schema[si].fields.len();
                        value_set[..nf].fill(false);
                        // Column 0 = first attribute (e.g. rdf:ID)
                        if let Some(attr_val) = first_attr {
                            clean_value_into(attr_val, &mut clean_buf);
                            value_slots[0].clear();
                            value_slots[0].extend_from_slice(&clean_buf);
                            value_set[0] = true;
                        } else {
                            value_slots[0].clear();
                            value_set[0] = true;
                        }
                    } else {
                        cur_schema = None;
                    }
                } else if depth >= 3 {
                    if let Some(si) = cur_schema {
                        if let Some(&col) = field_maps[si].get(name) {
                            if let Some(attr_val) = first_attr {
                                clean_value_into(attr_val, &mut clean_buf);
                                value_slots[col].clear();
                                value_slots[col].extend_from_slice(&clean_buf);
                                value_set[col] = true;
                            } else {
                                pending_col = Some(col);
                            }
                        }
                    }
                }
            }

            XmlEvent::Text(t) => {
                if let Some(col) = pending_col.take() {
                    clean_value_into(t, &mut clean_buf);
                    value_slots[col].clear();
                    value_slots[col].extend_from_slice(&clean_buf);
                    value_set[col] = true;
                }
            }

            // Empty events don't change depth:  depth==1 → record, depth>=2 → field.
            XmlEvent::Empty { name, first_attr } => {
                if depth == 1 {
                    // Self-closing record (child of root)
                    if let Some(&si) = schema_lookup.get(name) {
                        let nf = schema[si].fields.len();
                        value_set[..nf].fill(false);
                        if let Some(attr_val) = first_attr {
                            clean_value_into(attr_val, &mut clean_buf);
                            value_slots[0].clear();
                            value_slots[0].extend_from_slice(&clean_buf);
                            value_set[0] = true;
                        } else {
                            value_slots[0].clear();
                            value_set[0] = true;
                        }
                        write_row(&mut writers[si], schema[si].fields.len(), &value_slots, &value_set, &mut escape_buf, &mut row_buf);
                    }
                } else if depth >= 2 {
                    // Self-closing field (child of record)
                    if let Some(si) = cur_schema {
                        if let Some(&col) = field_maps[si].get(name) {
                            if let Some(attr_val) = first_attr {
                                clean_value_into(attr_val, &mut clean_buf);
                                value_slots[col].clear();
                                value_slots[col].extend_from_slice(&clean_buf);
                                value_set[col] = true;
                            } else {
                                value_slots[col].clear();
                                value_set[col] = true;
                            }
                        }
                    }
                }
            }

            XmlEvent::End => {
                if let Some(col) = pending_col.take() {
                    if cur_schema.is_some() {
                        value_slots[col].clear();
                        value_set[col] = true;
                    }
                }
                if depth == 2 {
                    if let Some(si) = cur_schema {
                        write_row(&mut writers[si], schema[si].fields.len(), &value_slots, &value_set, &mut escape_buf, &mut row_buf);
                    }
                    cur_schema = None;
                }
                depth -= 1;
            }

            XmlEvent::Eof => break,
        }
    }

    // Flush all writers
    for w in &mut writers {
        w.flush().map_err(|e| format!("Flush: {e}"))?;
    }
    Ok(())
}

#[inline]
fn write_row(
    writer: &mut BufWriter<fs::File>,
    num_fields: usize,
    values: &[Vec<u8>],
    set: &[bool],
    escape_buf: &mut Vec<u8>,
    row_buf: &mut Vec<u8>,
) {
    row_buf.clear();
    for col in 0..num_fields {
        if set[col] {
            csv_escape_into(&values[col], escape_buf);
            row_buf.extend_from_slice(escape_buf);
        }
        row_buf.push(b',');
    }
    row_buf.push(b'\n');
    let _ = writer.write_all(row_buf);
}

// ────────────────────────────────────────────────────────────────────────────
//  Orchestrator
// ────────────────────────────────────────────────────────────────────────────

pub fn run(
    xml_path: PathBuf,
    out_folder: PathBuf,
    mut progress: impl FnMut(Progress) + Send + 'static,
) {
    let start = Instant::now();
    let date_start = date_string();
    let time_start = time_string();

    // ── Read entire file into memory (single disk read) ──────────────────
    let data = match fs::read(&xml_path) {
        Ok(d) => d,
        Err(e) => {
            progress(Progress::Error(format!("Cannot read XML: {e}")));
            return;
        }
    };

    // ── Clear output folder ────────────────────────────────────────────────
    if let Ok(entries) = fs::read_dir(&out_folder) {
        for e in entries.flatten() {
            if e.path().is_file() {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    // ── Schema discovery ───────────────────────────────────────────────────
    let schema = match discover_schema(&data) {
        Ok(s) => s,
        Err(e) => {
            progress(Progress::Error(e));
            return;
        }
    };
    if let Err(e) = write_schema_csv(&schema, &out_folder.join("Schema List.csv")) {
        progress(Progress::Error(e));
        return;
    }

    // ── Data extraction ────────────────────────────────────────────────────
    if let Err(e) = extract_data(&data, &out_folder, &schema) {
        progress(Progress::Error(e));
        return;
    }

    // ── TEID fixup: convert 7-digit ConnectivityNode TEIDs to unique 6-digit ─
    match fix_teids(&out_folder) {
        Ok(msg) => {
            if !msg.is_empty() {
                progress(Progress::Log(msg));
            }
        }
        Err(e) => {
            progress(Progress::Log(format!("TEID fixup warning: {e}")));
        }
    }

    // ── File list ──────────────────────────────────────────────────────────
    if let Ok(mut f) = fs::File::create(out_folder.join("File List.CSV")) {
        let mut w = BufWriter::new(&mut f);
        for entry in &schema {
            let _ = w.write_all(&entry.record_type);
            let _ = w.write_all(b",\n");
        }
        let _ = w.flush();
    }

    // ── Extraction time ────────────────────────────────────────────────────
    if let Ok(mut f) = fs::File::create(out_folder.join("Extraction Time.CSV")) {
        let _ = writeln!(f, "{date_start}");
        let _ = writeln!(f, "{time_start}");
        let _ = writeln!(f, "{}", date_string());
        let _ = writeln!(f, "{}", time_string());
    }

    progress(Progress::Log(format!(
        "Finished in {:.1}s — {} record types extracted.",
        start.elapsed().as_secs_f64(),
        schema.len(),
    )));
    progress(Progress::Done);
}

// ────────────────────────────────────────────────────────────────────────────
//  Date / time (no chrono dependency)
// ────────────────────────────────────────────────────────────────────────────

fn date_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, m, d) = epoch_to_ymd(now);
    format!("{m:02}-{d:02}-{y:04}")
}

fn time_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = now % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn epoch_to_ymd(epoch: u64) -> (u32, u32, u32) {
    let days = (epoch / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as u32, m as u32, d as u32)
}
// ────────────────────────────────────────────────────────────────────────────
//  TEID fixup — convert 7-digit ConnectivityNode TEIDs to unique 6-digit
// ────────────────────────────────────────────────────────────────────────────

/// Read `cim-ConnectivityNode.CSV`, find the `etx:ConnectivityNode.teid` column,
/// and replace any 7-digit values with unique 6-digit values.
///
/// Strategy:
/// 1. Collect all existing 6-digit TEIDs into a HashSet.
/// 2. Group 7-digit TEIDs by their first-3-digit prefix.
/// 3. For each group, start with `(max_6digit / 10_000) + 10` as a candidate
///    2-digit prefix, wrapping at 99 -> 10.  Try every 2-digit prefix (10-99)
///    until one produces zero collisions with existing values or other
///    7-digit conversions.
/// 4. Replace just the TEID field in each affected row and write the file back.
fn fix_teids(out_folder: &Path) -> Result<String, String> {
    let path = out_folder.join("cim-ConnectivityNode.CSV");
    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(_) => return Ok(String::new()),
    };

    // Find header line & TEID column index
    let header_end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
    let header = &data[..header_end];
    let header_cols: Vec<&[u8]> = header.split(|&b| b == b',').collect();
    let teid_col = header_cols
        .iter()
        .position(|c| c == b"etx:ConnectivityNode.teid")
        .ok_or_else(|| "etx:ConnectivityNode.teid column not found".to_string())?;

    // Parse data rows: record (start, end) byte ranges & extract TEID
    let mut pos = header_end + 1;
    let mut row_ranges: Vec<(usize, usize)> = Vec::new();
    let mut teid_raw: Vec<&[u8]> = Vec::new();

    while pos < data.len() {
        let line_start = pos;
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        let line_end = pos;
        if pos < data.len() {
            pos += 1;
        }
        let line = &data[line_start..line_end];
        if line.is_empty() || line.iter().all(|&b| b == b'\r') {
            continue;
        }
        // Find the TEID field within this line
        let mut field_start = 0;
        let mut field_end = line.len();
        let mut commas = 0;
        for (j, &b) in line.iter().enumerate() {
            if b == b',' {
                if commas == teid_col {
                    field_end = j;
                    break;
                }
                commas += 1;
                field_start = j + 1;
            }
        }
        row_ranges.push((line_start, line_end));
        teid_raw.push(&line[field_start..field_end]);
    }

    // Separate 6-digit and 7-digit TEIDs
    let mut existing_6: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut seven_digit: Vec<(usize, u64)> = Vec::new();

    for (i, raw) in teid_raw.iter().enumerate() {
        if raw.is_empty() {
            continue;
        }
        if let Ok(s) = std::str::from_utf8(raw) {
            let s = s.trim();
            if let Ok(n) = s.parse::<u64>() {
                match s.len() {
                    6 => { existing_6.insert(n); }
                    7 => { seven_digit.push((i, n)); }
                    _ => {}
                }
            }
        }
    }

    if seven_digit.is_empty() {
        return Ok(String::new());
    }

    let count_7 = seven_digit.len();
    let max_6 = existing_6.iter().max().copied().unwrap_or(0);

    // Group 7-digit values by their first-3-digit prefix
    let mut prefix_groups: std::collections::HashMap<u64, Vec<(usize, u64)>> =
        std::collections::HashMap::new();
    for (i, val) in &seven_digit {
        let prefix = val / 10_000;
        prefix_groups.entry(prefix).or_default().push((*i, *val));
    }

    // For each prefix group, find a collision-free 2-digit replacement
    let mut replacements: Vec<Option<u64>> = vec![None; teid_raw.len()];

    for (old_prefix, group) in &prefix_groups {
        // Start one above the max 6-digit's first 2 digits, wrap at 99 -> 10.
        // The collision-checking loop below will try every 2-digit prefix
        // (10-99) until a collision-free one is found.
        let mut candidate = (max_6 / 10_000) + 1;
        if candidate >= 100 {
            candidate = 10;
        }

        let mut found = false;
        for _ in 0..90 {
            let mut collision = false;
            let mut used: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for (_, val) in group {
                let last4 = val % 10_000;
                let new_val = candidate * 10_000 + last4;
                if existing_6.contains(&new_val) || used.contains(&new_val) {
                    collision = true;
                    break;
                }
                used.insert(new_val);
            }
            if !collision {
                for (i, val) in group {
                    let last4 = val % 10_000;
                    let new_val = candidate * 10_000 + last4;
                    replacements[*i] = Some(new_val);
                    existing_6.insert(new_val);
                }
                found = true;
                break;
            }
            candidate += 1;
            if candidate >= 100 {
                candidate = 10;
            }
        }
        if !found {
            return Err(format!(
                "Could not find a collision-free 2-digit prefix for 7-digit TEIDs starting with {old_prefix}"
            ));
        }
    }

    // Rewrite the file, replacing only the TEID field in affected rows
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[..header_end + 1]); // header

    for (row_idx, (start, end)) in row_ranges.iter().enumerate() {
        if let Some(new_val) = replacements[row_idx] {
            let line = &data[*start..*end];
            let mut field_start = 0;
            let mut field_end = line.len();
            let mut commas = 0;
            for (j, &b) in line.iter().enumerate() {
                if b == b',' {
                    if commas == teid_col {
                        field_end = j;
                        break;
                    }
                    commas += 1;
                    field_start = j + 1;
                }
            }
            out.extend_from_slice(&line[..field_start]);
            out.extend_from_slice(new_val.to_string().as_bytes());
            out.extend_from_slice(&line[field_end..]);
        } else {
            out.extend_from_slice(&data[*start..*end]);
        }
        out.push(b'\n');
    }

    fs::write(&path, &out).map_err(|e| format!("Write cim-ConnectivityNode.CSV: {e}"))?;

    let prefix_summary: Vec<String> = prefix_groups.keys().map(|p| p.to_string()).collect();
    Ok(format!(
        "Fixed {count_7} 7-digit TEIDs (prefixes: {}) -> 6-digit",
        prefix_summary.join(", ")
    ))
}
