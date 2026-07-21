//! Shared Table struct and helpers — used by both model_builder and sswg_mapping.

use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::rc::Rc;

pub type S = Rc<str>;
pub fn rc<T: AsRef<str>>(v: T) -> S { Rc::from(v.as_ref()) }

pub struct Table {
    pub headers: Vec<S>,
    pub hmap: HashMap<S, usize>,
    pub rows: Vec<Vec<S>>,
}

impl Table {
    pub fn new() -> Self { Table { headers: vec![], hmap: HashMap::new(), rows: vec![] } }

    /// Borrowed read — zero allocation. Use this in hot loops.
    pub fn gr(&self, r: usize, c: usize) -> &str {
        if c == usize::MAX { return ""; }
        self.rows.get(r).and_then(|row| row.get(c)).map(|v| v.as_ref()).unwrap_or("")
    }

    pub fn load(path: &Path, prefix: &str) -> Result<Self, String> {
        let data = fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let mut t = Table::new();
        let mut lines = data.lines();
        if let Some(h) = lines.next() {
            for f in h.split(',') {
                let name: S = if f.is_empty() { rc(format!("{prefix}-Dummy{prefix}")) } else { rc(format!("{prefix}-{f}")) };
                let i = t.headers.len(); t.headers.push(name.clone()); t.hmap.insert(name, i);
            }
        }
        for l in lines { if !l.is_empty() { t.rows.push(l.split(',').map(|f| rc(f)).collect()); } }
        Ok(t)
    }

    pub fn g(&self, r: usize, c: usize) -> String {
        if c == usize::MAX { return String::new(); }
        self.rows.get(r).and_then(|row| row.get(c)).map(|v| v.to_string()).unwrap_or_default()
    }

    pub fn c(&self, n: &str) -> usize { self.hmap.get(n).copied().unwrap_or(usize::MAX) }
    pub fn has(&self, n: &str) -> bool { self.hmap.contains_key(n) }
    pub fn s(&mut self, r: usize, c: usize, v: impl AsRef<str>) {
        if c == usize::MAX { return; }
        while r >= self.rows.len() { self.rows.push(vec![]); }
        while c >= self.rows[r].len() { self.rows[r].push(rc("")); }
        self.rows[r][c] = rc(v.as_ref());
    }
    /// O(1) write — clones the Rc<str> directly, no string allocation.
    pub fn s_rc(&mut self, r: usize, c: usize, v: S) {
        if c == usize::MAX { return; }
        while r >= self.rows.len() { self.rows.push(vec![]); }
        while c >= self.rows[r].len() { self.rows[r].push(rc("")); }
        self.rows[r][c] = v;
    }
    pub fn add_col(&mut self, n: &str) -> usize {
        let ns = rc(n);
        if let Some(&i) = self.hmap.get(&ns) { return i; }
        let i = self.headers.len(); self.headers.push(ns.clone()); self.hmap.insert(ns, i);
        for r in &mut self.rows { r.push(rc("")); } i
    }
    pub fn add_row(&mut self) -> usize { let i = self.rows.len(); self.rows.push(vec![rc(""); self.headers.len()]); i }
    pub fn n(&self) -> usize { self.rows.len() }
    pub fn clone_t(&self) -> Self { Table { headers: self.headers.clone(), hmap: self.hmap.clone(), rows: self.rows.clone() } }

    pub fn index(&self, col: usize) -> HashMap<S, Vec<usize>> {
        let mut m: HashMap<S, Vec<usize>> = HashMap::new();
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(v) = row.get(col) { m.entry(v.clone()).or_default().push(i); }
        }
        m
    }

    pub fn write_csv(&self, path: &Path) -> Result<(), String> {
        let mut w = BufWriter::with_capacity(256 * 1024, fs::File::create(path).map_err(|e| format!("Create {}: {e}", path.display()))?);
        for (i, h) in self.headers.iter().enumerate() {
            if i > 0 { w.write_all(b",").map_err(|e| format!("W: {e}"))?; }
            w.write_all(h.as_bytes()).map_err(|e| format!("W: {e}"))?;
        }
        w.write_all(b",\n").map_err(|e| format!("W: {e}"))?;
        let mut buf = Vec::with_capacity(8192);
        for r in &self.rows {
            buf.clear();
            for (i, v) in r.iter().enumerate() {
                if i > 0 { buf.push(b','); }
                buf.extend_from_slice(v.as_bytes());
            }
            buf.extend_from_slice(b",\n");
            w.write_all(&buf).map_err(|e| format!("W: {e}"))?;
        }
        w.flush().map_err(|e| format!("F: {e}"))?; Ok(())
    }
}

/// Join parent to child: add all child columns to parent, copy matching rows.
pub fn j(parent: &mut Table, child: &Table, pk: &str, ck: &str) -> Result<(), String> {
    let pc = parent.c(pk);
    let cc = child.c(ck);
    if pc == usize::MAX { return Err(format!("Key '{pk}' missing in parent")); }
    if cc == usize::MAX { return Err(format!("Key '{ck}' missing in child")); }
    let child_cols: Vec<(usize, usize)> = child.headers.iter().enumerate().map(|(i, h)| (i, parent.add_col(h))).collect();
    let idx = child.index(cc);
    for pr in 0..parent.n() {
        if let Some(crs) = idx.get(parent.gr(pr, pc)) {
            for &cr in crs { for &(cc2, pc2) in &child_cols { parent.s_rc(pr, pc2, child.rows[cr][cc2].clone()); } }
        }
    }
    Ok(())
}

/// Selective join: only copy specified columns from child to parent.
pub fn jc(parent: &mut Table, child: &Table, pk: &str, ck: &str, cols: &[&str]) -> Result<(), String> {
    let pc = parent.c(pk);
    let cc = child.c(ck);
    if pc == usize::MAX { return Err(format!("Key '{pk}' missing in parent")); }
    if cc == usize::MAX { return Err(format!("Key '{ck}' missing in child")); }
    let child_cols: Vec<(usize, usize)> = cols.iter().filter_map(|col| {
        let cc2 = child.c(col);
        if cc2 == usize::MAX { return None; }
        Some((cc2, parent.add_col(col)))
    }).collect();
    let idx = child.index(cc);
    for pr in 0..parent.n() {
        if let Some(crs) = idx.get(parent.gr(pr, pc)) {
            for &cr in crs { for &(cc2, pc2) in &child_cols { parent.s_rc(pr, pc2, child.rows[cr][cc2].clone()); } }
        }
    }
    Ok(())
}

pub fn parse_f(val: &str) -> f64 { val.trim().parse::<f64>().unwrap_or(0.0) }
#[allow(dead_code)]
pub fn parse_i(val: &str) -> i64 { val.trim().parse::<i64>().unwrap_or(0) }
impl Table {
    /// Clean ConductingEquipment column: strip { } and trim.
    pub fn clean_ce(&mut self) {
        let src = self.c("cim-Terminal-cim:Terminal.ConductingEquipment");
        let dst = self.add_col("cim-Terminal-cim:Terminal.ConductingEquipment Clean");
        for i in 0..self.n() {
            let v = rc(self.gr(i, src).replace("{", "").replace("}", "").trim());
            self.s_rc(i, dst, v);
        }
    }
    /// Clean a column: strip { } and trim, write to "{src} Clean".
    pub fn clean_col(&mut self, src: &str) {
        let sc = self.c(src);
        let dst = self.add_col(&format!("{src} Clean"));
        for i in 0..self.n() {
            let v = rc(self.gr(i, sc).replace("{", "").replace("}", "").trim());
            self.s_rc(i, dst, v);
        }
    }
}
