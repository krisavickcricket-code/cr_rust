//! Power system model builder — Rust port of CR2/Module1.vb
//!
//! All O(N²) GetChildRows nested-loop joins replaced with HashMap-indexed O(1) lookups.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

type S = Rc<str>; // Reference-counted shared string — clone is O(1), like VB's string assignment
fn rc<T: AsRef<str>>(v: T) -> S { Rc::from(v.as_ref()) }

#[derive(Debug, Clone)]
pub enum Progress {
    Log(String),
    Done,
    Error(String),
}

// ── Table ──────────────────────────────────────────────────────────────

struct Table {
    headers: Vec<S>,
    hmap: HashMap<S, usize>,
    rows: Vec<Vec<S>>,
}

impl Table {
    fn new() -> Self { Table { headers: vec![], hmap: HashMap::new(), rows: vec![] } }

    /// Borrowed read — zero allocation. Use this in hot loops.
    fn gr(&self, r: usize, c: usize) -> &str {
        if c == usize::MAX { return ""; }
        self.rows.get(r).and_then(|row| row.get(c)).map(|v| v.as_ref()).unwrap_or("")
    }

    fn load(path: &Path, prefix: &str) -> Result<Self, String> {
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

    fn g(&self, r: usize, c: usize) -> String {
        if c == usize::MAX { return String::new(); }
        self.rows.get(r).and_then(|row| row.get(c)).map(|v| v.to_string()).unwrap_or_default()
    }

    fn c(&self, n: &str) -> usize { self.hmap.get(n).copied().unwrap_or(usize::MAX) }
    fn has(&self, n: &str) -> bool { self.hmap.contains_key(n) }
    fn s(&mut self, r: usize, c: usize, v: impl AsRef<str>) {
        if c == usize::MAX { return; } // guard: column not found
        while r >= self.rows.len() { self.rows.push(vec![]); }
        while c >= self.rows[r].len() { self.rows[r].push(rc("")); }
        self.rows[r][c] = rc(v.as_ref());
    }
    /// O(1) write — clones the Rc<str> directly, no string allocation.
    fn s_rc(&mut self, r: usize, c: usize, v: S) {
        if c == usize::MAX { return; }
        while r >= self.rows.len() { self.rows.push(vec![]); }
        while c >= self.rows[r].len() { self.rows[r].push(rc("")); }
        self.rows[r][c] = v;
    }
    fn add_col(&mut self, n: &str) -> usize {
        let ns = rc(n);
        if let Some(&i) = self.hmap.get(&ns) { return i; }
        let i = self.headers.len(); self.headers.push(ns.clone()); self.hmap.insert(ns, i);
        for r in &mut self.rows { r.push(rc("")); } i
    }
    fn add_row(&mut self) -> usize { let i = self.rows.len(); self.rows.push(vec![rc(""); self.headers.len()]); i }
    fn n(&self) -> usize { self.rows.len() }
    fn clone_t(&self) -> Self { Table { headers: self.headers.clone(), hmap: self.hmap.clone(), rows: self.rows.clone() } }

    fn index(&self, col: usize) -> HashMap<S, Vec<usize>> {
        let mut m: HashMap<S, Vec<usize>> = HashMap::new();
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(v) = row.get(col) { m.entry(v.clone()).or_default().push(i); }
        }
        m
    }

    fn write_csv(&self, path: &Path) -> Result<(), String> {
        let mut w = BufWriter::with_capacity(256 * 1024, fs::File::create(path).map_err(|e| format!("Create {}: {e}", path.display()))?);
        for (i, h) in self.headers.iter().enumerate() {
            if i > 0 { w.write_all(b",").map_err(|e| format!("W: {e}"))?; }
            w.write_all(h.as_bytes()).map_err(|e| format!("W: {e}"))?;
        }
        w.write_all(b",
").map_err(|e| format!("W: {e}"))?;
        let mut buf = Vec::with_capacity(8192);
        for r in &self.rows {
            buf.clear();
            for (i, v) in r.iter().enumerate() {
                if i > 0 { buf.push(b','); }
                buf.extend_from_slice(v.as_bytes());
            }
            buf.extend_from_slice(b",
");
            w.write_all(&buf).map_err(|e| format!("W: {e}"))?;
        }
        w.flush().map_err(|e| format!("F: {e}"))?; Ok(())
    }
}

/// Join parent to child: add all child columns to parent, copy matching rows.
fn j(parent: &mut Table, child: &Table, pk: &str, ck: &str) -> Result<(), String> {
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
/// Dramatically reduces memory for large parent tables.
fn jc(parent: &mut Table, child: &Table, pk: &str, ck: &str, cols: &[&str]) -> Result<(), String> {
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

fn parse_f(val: &str) -> f64 { val.trim().parse::<f64>().unwrap_or(0.0) }
#[allow(dead_code)]
fn parse_i(val: &str) -> i64 { val.trim().parse::<i64>().unwrap_or(0) }

// ── Build model ────────────────────────────────────────────────────────

pub fn run(input_folder: PathBuf, progress: impl FnMut(Progress) + Send + 'static) {
    let mut progress = progress;
    let start = Instant::now();
    let out = input_folder.join("Output");
    let _ = fs::create_dir_all(&out);
    let logpath = out.join("build_log.txt");
    let _ = fs::write(&logpath, "Build log started\n");
    let mut log = |m: String| {
        progress(Progress::Log(m.clone()));
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&logpath) { let _ = writeln!(f, "{m}"); }
    };

    macro_rules! ld {
        ($f:expr, $p:expr) => { match Table::load(&input_folder.join($f), $p) { Ok(t) => t, Err(e) => { progress(Progress::Error(e)); return; } } };
    }

    // ── Load all CSVs ──
    let cn  = ld!("cim-ConnectivityNode.csv", "cim-ConnectivityNode");
    let cng = ld!("etx-ConnectivityNodeGroup.csv", "etx-ConnectivityNodeGroup");
    let eb  = ld!("etx-ElectricalBus.csv", "etx-ElectricalBus");
    let mut vl  = ld!("cim-VoltageLevel.csv", "cim-VoltageLevel");
    let mut t   = ld!("cim-Terminal.csv", "cim-Terminal");
    let mut hb  = ld!("etx-HUBBus.csv", "etx-HUBBus");
    let mut sh  = ld!("etx-SettlementHUB.csv", "etx-SettlementHUB");
    let mut col = ld!("cim-ConformLoadGroup.csv", "cim-ConformLoadGroup");
    let mut cul = ld!("cim-CustomerLoad.csv", "cim-CustomerLoad");
    let mut sl  = ld!("cim-SubLoadArea.csv", "cim-SubLoadArea");
    let ss  = ld!("cim-Substation.csv", "cim-Substation");
    let mut lz  = ld!("etx-SettlementLoadZone.csv", "etx-SettlementLoadZone");
    let mut no  = ld!("etx-SettlementNOIELoadZone.csv", "etx-SettlementNOIELoadZone");
    let mut dm  = ld!("cim-DAM.csv", "cim-Dam");
    let mut al  = ld!("cim-AnalogLimit.csv", "cim-AnalogLimit");
    let l   = ld!("cim-Line.csv", "cim-Line");
    let mut ac  = ld!("cim-ACLineSegment.csv", "cim-ACLineSegment");
    let mut r   = ld!("etx-Rating.csv", "etx-Rating");
    let o   = ld!("etx-OwnerShareRating.csv", "etx-OwnerShareRating");
    let b   = ld!("cim-Breaker.csv", "cim-Breaker");
    let d   = ld!("cim-Disconnector.csv", "cim-Disconnector");
    let mut tw  = ld!("cim-TransformerWinding.csv", "cim-TransformerWinding");
    let pt  = ld!("cim-PowerTransformer.csv", "cim-PowerTransformer");
    let mut al1 = ld!("cim-AnalogLimit.csv", "cim-AnalogLimit");
    let mut r1  = ld!("etx-Rating.csv", "etx-Rating");
    let sc  = ld!("cim-ShuntCompensator.csv", "cim-ShuntCompensator");
    let rn  = ld!("etx-ResourceNode.csv", "etx-ResourceNode");
    let mut tg  = ld!("cim-ThermalGeneratingUnit.csv", "cim-ThermalGeneratingUnit");
    let mut wg  = ld!("etx-WindGeneratingUnit.csv", "etx-WindGeneratingUnit");
    let mut ng  = ld!("etx-NuclearGeneratingUnit.csv", "etx-NuclearGeneratingUnit");
    let mut hg  = ld!("cim-HydroGeneratingUnit.csv", "cim-HydroGeneratingUnit");
    let sm  = ld!("cim-SynchronousMachine.csv", "cim-SynchronousMachine");
    let mut sg  = ld!("etx-SolarGeneratingUnit.csv", "etx-SolarGeneratingUnit");
    let sec = ld!("cim-SeriesCompensator.csv", "cim-SeriesCompensator");
    let mut dc  = ld!("etx-DCTie.csv", "etx-DCTie");

    log(format!("CSVs loaded: T={} CN={} AC={} TW={} PT={} CUL={} DM={} D={} B={}", t.n(), cn.n(), ac.n(), tw.n(), pt.n(), cul.n(), dm.n(), d.n(), b.n()));
    log("Transforming…".to_string());

    // ── Numbering hubs ──
    let hid = sh.add_col("HubID");
    for i in 0..sh.n() { sh.s(i, hid, rc((i + 2).to_string())); }

    // ── Demand zones + weather zone mapping ──
    let dz = sl.add_col("Demand Zone ID"); sl.add_col("Total Load");
    let dummy = if sl.has("cim-SubLoadArea-Dummy SL") { sl.c("cim-SubLoadArea-Dummy SL") } else { sl.add_col("cim-SubLoadArea-Dummy SL") };
    let sl_name = sl.c("cim-SubLoadArea-cim:IdentifiedObject.name");
    let fr = sl.add_row(); sl.s(fr, sl_name, "FLAT".to_string());
    let mut zid = 1usize; let mut zidflat = 0usize;
    for i in 0..sl.n() {
        zid += 1; sl.s(i, dz, rc(zid.to_string()));
        let m = match sl.g(i, sl_name).as_ref() {
            "WXZNRTHC" => "NORTHCEN", "WXZSTHC" => "SOUTHCEN", "WXZWEST" => "WEST",
            "WXZCOAST" => "COAST", "WXZNORTH" => "NORTH", "WXZFWEST" => "FARWEST",
            "WXZSTHRN" => "SOUTHERN", "WXZEAST" => "EAST", "FLAT" => "FLAT", _ => "",
        };
        if !m.is_empty() { sl.s(i, dummy, m.to_string()); if m == "FLAT" { zidflat = zid; } }
    }

    // ── Load zones ──
    let lzid = lz.add_col("Load Zone ID"); let mut lid = 1usize;
    for i in 0..lz.n() { lid += 1; lz.s(i, lzid, rc(lid.to_string())); }

    // ── NOIE zones ──
    let nid = no.add_col("NOIE Load Zone ID");
    for i in 0..no.n() { lid += 1; no.s(i, nid, rc(lid.to_string())); }

    dm.add_col("DM Load Found");
    rework_al(&mut al); rework_al(&mut al1);

    log("Joins…".to_string());

    // ── Joins ──
    macro_rules! J { ($p:expr, $c:expr, $pk:expr, $ck:expr) => { if let Err(e) = j(&mut $p, &$c, $pk, $ck) { progress(Progress::Error(e)); return; } } }
    macro_rules! JC { ($p:expr, $c:expr, $pk:expr, $ck:expr, [$($col:expr),*]) => { if let Err(e) = jc(&mut $p, &$c, $pk, $ck, &[$($col),*]) { progress(Progress::Error(e)); return; } } }

    log("Join T-CN…".to_string());

    JC!(t, cn, "cim-Terminal-cim:Terminal.ConnectivityNode", "cim-ConnectivityNode-cim:ConnectivityNode", ["cim-ConnectivityNode-cim:ConnectivityNode", "cim-ConnectivityNode-etx:ConnectivityNode.teid", "cim-ConnectivityNode-cim:IdentifiedObject.name", "cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup"]);
    log("Join T-CN-CNG…".to_string());
    JC!(t, cng, "cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup", ["etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber", "etx-ConnectivityNodeGroup-etx:PlanningBay.VoltageLevel", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.HasAHUBBus"]);
    log("Join VL-SS, VL-LZ…".to_string());
    JC!(vl, ss, "cim-VoltageLevel-cim:VoltageLevel.MemberOf_Substation", "cim-Substation-cim:Substation", ["cim-Substation-cim:IdentifiedObject.name", "cim-Substation-cim:IdentifiedObject.description", "cim-Substation-etx:Substation.SettlementLoadZone"]);
    JC!(vl, lz, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementLoadZone-etx:SettlementLoadZone", ["Load Zone ID", "etx-SettlementLoadZone-cim:IdentifiedObject.name"]);
    log("Join T-VL…".to_string());
    JC!(t, vl, "etx-ConnectivityNodeGroup-etx:PlanningBay.VoltageLevel", "cim-VoltageLevel-cim:VoltageLevel", ["cim-VoltageLevel-cim:IdentifiedObject.name", "cim-Substation-cim:IdentifiedObject.name", "cim-Substation-cim:IdentifiedObject.description", "Load Zone ID", "etx-SettlementLoadZone-cim:IdentifiedObject.name"]);
    log("Join T-EB…".to_string());
    JC!(t, eb, "cim-ConnectivityNode-cim:ConnectivityNode", "etx-ElectricalBus-etx:ElectricalBus.ConnectivityNode", ["etx-ElectricalBus-cim:IdentifiedObject.name", "etx-ElectricalBus-etx:ElectricalBus"]);
    log("Join HB-SH…".to_string());
    JC!(hb, sh, "etx-HUBBus-etx:HUBBus.SettlementHub", "etx-SettlementHUB-etx:SettlementHUB", ["HubID", "etx-SettlementHUB-cim:IdentifiedObject.name"]);
    log("Join T-HB-SH…".to_string());
    JC!(t, hb, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.HasAHUBBus", "etx-HUBBus-etx:HUBBus", ["HubID", "etx-SettlementHUB-cim:IdentifiedObject.name"]);
    log("Clean CE…".to_string());
    t.clean_ce();
    log("Join COL-SL, CUL-COL…".to_string());
    JC!(col, sl, "cim-ConformLoadGroup-cim:LoadGroup.SubLoadArea", "cim-SubLoadArea-cim:SubLoadArea", ["Demand Zone ID", "cim-SubLoadArea-Dummy SL"]);
    cul.clean_col("cim-CustomerLoad-cim:CustomerLoad");
    JC!(cul, col, "cim-CustomerLoad-cim:ConformLoad.LoadGroup", "cim-ConformLoadGroup-cim:ConformLoadGroup", ["Demand Zone ID", "cim-SubLoadArea-Dummy SL"]);
    dm.clean_col("cim-Dam-MRIDLoad");
    JC!(cul, dm, "cim-CustomerLoad-cim:CustomerLoad Clean", "cim-Dam-MRIDLoad Clean", ["cim-Dam-DistributionFactor", "cim-Dam-MVARDistributionFactor", "cim-Dam-SubStation", "cim-Dam-MRIDLoad"]);

    // Mark DM Load Found + Map NOIEs via HashMap
    {
        let dm_clean = dm.c("cim-Dam-MRIDLoad Clean");
        let dm_found = dm.c("DM Load Found");
        let cul_clean = cul.c("cim-CustomerLoad-cim:CustomerLoad Clean");
        let di = dm.index(dm_clean);
        for i in 0..cul.n() {
            if let Some(rs) = di.get(cul.gr(i, cul_clean)) { for &r in rs { dm.s(r, dm_found, "1".to_string()); } }
        }
        let no_idx = no.index(no.c("etx-SettlementNOIELoadZone-etx:SettlementNOIELoadZone"));
        let cn1 = cul.add_col("etx-SettlementNoieLoadZone-cim:IdentifiedObject.name");
        let cn2 = cul.add_col("NOIE Load Zone ID");
        let ck = cul.c("cim-CustomerLoad-etx:EnergyConsumer.SettlementNOIELoadZone");
        let non = no.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name");
        for i in 0..cul.n() {
            if let Some(rs) = no_idx.get(cul.gr(i, ck)) {
                if let Some(&r) = rs.first() { cul.s(i, cn1, no.gr(r, non)); cul.s(i, cn2, no.gr(r, nid)); }
            }
        }
    }
    JC!(t, cul, "cim-Terminal-cim:Terminal.ConductingEquipment", "cim-CustomerLoad-cim:CustomerLoad", ["cim-CustomerLoad-cim:CustomerLoad", "NOIE Load Zone ID", "Demand Zone ID", "cim-SubLoadArea-Dummy SL", "etx-SettlementNoieLoadZone-cim:IdentifiedObject.name"]);
    JC!(ac, l, "cim-ACLineSegment-cim:Equipment.MemberOf_EquipmentContainer", "cim-Line-cim:Line", ["cim-Line-cim:IdentifiedObject.name"]);
    JC!(ac, o, "cim-ACLineSegment-cim:ACLineSegment", "etx-OwnerShareRating-etx:OwnerShareRating.Equipment", ["etx-OwnerShareRating-etx:OwnerShareRating"]);
    JC!(r, ac, "etx-Rating-etx:Rating.OwnerShareRating", "etx-OwnerShareRating-etx:OwnerShareRating", ["cim-ACLineSegment-cim:ACLineSegment", "cim-Line-cim:IdentifiedObject.name", "cim-ACLineSegment-cim:IdentifiedObject.name"]);
    JC!(al, r, "cim-AnalogLimit-cim:AnalogLimit.LimitSet", "etx-Rating-etx:Rating", ["etx-Rating-cim:IdentifiedObject.name", "cim-ACLineSegment-cim:ACLineSegment", "cim-Line-cim:IdentifiedObject.name", "cim-ACLineSegment-cim:IdentifiedObject.name"]);
    // TW-PT join skipped: PT columns in TW are never accessed
    JC!(tw, o, "cim-TransformerWinding-cim:TransformerWinding", "etx-OwnerShareRating-etx:OwnerShareRating.Equipment", ["etx-OwnerShareRating-etx:OwnerShareRating"]);
    JC!(r1, tw, "etx-Rating-etx:Rating.OwnerShareRating", "etx-OwnerShareRating-etx:OwnerShareRating", ["cim-TransformerWinding-cim:TransformerWinding.MemberOf_PowerTransformer", "etx-OwnerShareRating-etx:OwnerShareRating"]);
    JC!(al1, r1, "cim-AnalogLimit-cim:AnalogLimit.LimitSet", "etx-Rating-etx:Rating", ["etx-Rating-cim:IdentifiedObject.name", "cim-TransformerWinding-cim:TransformerWinding.MemberOf_PowerTransformer"]);
    J!(tg, sm, "cim-ThermalGeneratingUnit-cim:ThermalGeneratingUnit", "cim-SynchronousMachine-cim:SynchronousMachine.MemberOf_GeneratingUnit");
    J!(wg, sm, "etx-WindGeneratingUnit-etx:WindGeneratingUnit", "cim-SynchronousMachine-cim:SynchronousMachine.MemberOf_GeneratingUnit");
    J!(sg, sm, "etx-SolarGeneratingUnit-etx:SolarGeneratingUnit", "cim-SynchronousMachine-cim:SynchronousMachine.MemberOf_GeneratingUnit");
    J!(ng, sm, "etx-NuclearGeneratingUnit-etx:NuclearGeneratingUnit", "cim-SynchronousMachine-cim:SynchronousMachine.MemberOf_GeneratingUnit");
    J!(hg, sm, "cim-HydroGeneratingUnit-cim:HydroGeneratingUnit", "cim-SynchronousMachine-cim:SynchronousMachine.MemberOf_GeneratingUnit");
    log(format!("All joins done. T now has {} cols, {} rows. Freeing memory…", t.headers.len(), t.n()));
    // Only drop tables confirmed not used after this point
    drop(sm); drop(cng); drop(vl); drop(eb); drop(hb); drop(ss); drop(col);
    drop(l); drop(o); drop(r); drop(r1);
    log(format!("Building output tables… T={} rows {} cols", t.n(), t.headers.len()));

    // ── Terminal CE index (used by many lookups) ──
    log("Building Terminal CE index…".to_string());
    let t_ce_idx = t.index(t.c("cim-Terminal-cim:Terminal.ConductingEquipment"));
    log("Building Terminal CN index…".to_string());
    let t_cn_idx = t.index(t.c("cim-ConnectivityNode-cim:ConnectivityNode"));
    log("Terminal indexes done.".to_string());

    // ── Resource Node Data ──
    let mut rndata = Table::new();
    for c in ["Resource Node Identifier", "Object Identifier", "Resource Node Name", "Bus Number"] { rndata.add_col(c); }
    let rn_eb = t.index(t.c("etx-ElectricalBus-etx:ElectricalBus"));
    log("rn_eb index done. Building Resource Node Data...".to_string());
    for i in 0..rn.n() {
        let r = rndata.add_row();
        rndata.s(r, 0, rn.gr(i, rn.c("etx-ResourceNode-etx:ResourceNode")));
        rndata.s(r, 1, rn.gr(i, rn.c("etx-ResourceNode-cim:IdentifiedObject.description")));
        rndata.s(r, 2, rn.gr(i, rn.c("etx-ResourceNode-cim:IdentifiedObject.name")));
        if let Some(rs) = rn_eb.get(rn.gr(i, rn.c("etx-ResourceNode-etx:ResourceNode.ElectricalBus"))) {
            if let Some(&tr) = rs.first() { rndata.s(r, 3, t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid"))); }
        }
    }

    log("Building Bus Data...".to_string());
    // ── Bus Data ──
    let mut busdata = Table::new();
    for c in ["Connectivity Node", "Bus Number", "Bus Name", "Base KV", "IDE", "Area", "Zone", "Owner", "VM", "VA", "Substation", "Full Substation", "Hub"] { busdata.add_col(c); }
    for i in 0..cn.n() {
        let r = busdata.add_row();
        busdata.s(r, 0, cn.gr(i, cn.c("cim-ConnectivityNode-cim:ConnectivityNode")));
        busdata.s(r, 1, cn.gr(i, cn.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
    }
    for i in 0..busdata.n() {
        if let Some(rs) = t_cn_idx.get(busdata.gr(i, 0)) {
            if let Some(&tr) = rs.first() {
                let ebn = t.gr(tr, t.c("etx-ElectricalBus-cim:IdentifiedObject.name"));
                if ebn.is_empty() {
                    let n = t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName"));
                    let cnn = t.gr(tr, t.c("cim-ConnectivityNode-cim:IdentifiedObject.name"));
                    busdata.s(i, 2, format!("'{}_{}'", n, cnn));
                } else { busdata.s(i, 2, format!("'{}'", ebn)); }
                busdata.s(i, 3, t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                busdata.s(i, 4, "1".to_string());
                let noie = t.gr(tr, t.c("NOIE Load Zone ID"));
                if noie.is_empty() {
                    let lzid_v = t.gr(tr, t.c("Load Zone ID"));
                    busdata.s(i, 5, if lzid_v.is_empty() { "1" } else { lzid_v });
                } else { busdata.s(i, 5, noie); }
                busdata.s(i, 10, t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                busdata.s(i, 11, t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                let dzv = t.gr(tr, t.c("Demand Zone ID"));
                busdata.s(i, 6, if dzv.is_empty() { "1" } else { dzv });
                let hidv = t.gr(tr, t.c("HubID"));
                let hubv = t.gr(tr, t.c("etx-SettlementHUB-cim:IdentifiedObject.name"));
                if hidv.is_empty() { busdata.s(i, 7, "1".to_string()); busdata.s(i, 12, "1".to_string()); }
                else { busdata.s(i, 7, hidv); busdata.s(i, 12, hubv); }
                busdata.s(i, 8, "1".to_string()); busdata.s(i, 9, "0".to_string());
            }
        }
    }

    log("Building Shunt Data...".to_string());
    // ── Shunt Data ──
    let mut shuntdata = Table::new();
    for c in ["Shunt", "Identifier", "IDname", "Name", "Bus Number", "Bus Voltage", "ID", "Status", "GL", "BL", "BL PU"] { shuntdata.add_col(c); }
    for i in 0..sc.n() {
        let r = shuntdata.add_row();
        shuntdata.s(r, 0, sc.gr(i, sc.c("cim-ShuntCompensator-cim:ShuntCompensator")));
        shuntdata.s(r, 1, sc.gr(i, sc.c("cim-ShuntCompensator-cim:IdentifiedObject.name")));
        shuntdata.s(r, 2, sc.gr(i, sc.c("cim-ShuntCompensator-etx:PowerSystemResource.teid")));
        shuntdata.s(r, 6, "1".to_string()); shuntdata.s(r, 7, "1".to_string()); shuntdata.s(r, 8, "0".to_string());
        shuntdata.s(r, 9, sc.gr(i, sc.c("cim-ShuntCompensator-cim:ShuntCompensator.nominalMVAr")));
        if let Some(rs) = t_ce_idx.get(shuntdata.gr(r, 0)) {
            if let Some(&tr) = rs.first() {
                shuntdata.s(r, 4, t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                shuntdata.s(r, 3, format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), shuntdata.gr(r, 1)));
                shuntdata.s(r, 5, t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
            }
        }
        let bl = parse_f(shuntdata.gr(r, 9)); let bv = parse_f(shuntdata.gr(r, 5));
        shuntdata.s(r, 10, (bl * bv * bv / 100.0).to_string());
    }

    log("Building Generation Data...".to_string());
    // ── Generation Data ──
    let mut gendata = Table::new();
    for c in ["Generator Name","Generator Type","Machine ID","Gen ID","Identifier Object Name","Machine Identifier Object Name","TEID","Machine TEID","Bus Number","ID","PG","QG","QT","QB","Vs","IREG","MBASE","ZR","ZX","RT","XT","GTAP","STAT","RMPCT","PT","PB","O1","F1","O2","F2","O3","F3","O4","F4","WMOD","WPF"] { gendata.add_col(c); }
    pop_gen(&mut gendata, &hg, "cim-HydroGeneratingUnit-cim:HydroGeneratingUnit", "cim-HydroGeneratingUnit-cim:IdentifiedObject.name", "cim-HydroGeneratingUnit-etx:PowerSystemResource.teid", "cim-HydroGeneratingUnit-cim:GeneratingUnit.initialMW", "cim-HydroGeneratingUnit-etx:GeneratingUnit.highReasonabilityLimit", "cim-HydroGeneratingUnit-etx:GeneratingUnit.lowReasonabilityLimit", "HYDRO");
    pop_gen(&mut gendata, &tg, "cim-ThermalGeneratingUnit-cim:ThermalGeneratingUnit", "cim-ThermalGeneratingUnit-cim:IdentifiedObject.name", "cim-ThermalGeneratingUnit-etx:PowerSystemResource.teid", "cim-ThermalGeneratingUnit-cim:GeneratingUnit.initialMW", "cim-ThermalGeneratingUnit-etx:GeneratingUnit.highReasonabilityLimit", "cim-ThermalGeneratingUnit-etx:GeneratingUnit.lowReasonabilityLimit", "THERMAL");
    pop_gen(&mut gendata, &ng, "etx-NuclearGeneratingUnit-etx:NuclearGeneratingUnit", "etx-NuclearGeneratingUnit-cim:IdentifiedObject.name", "etx-NuclearGeneratingUnit-etx:PowerSystemResource.teid", "etx-NuclearGeneratingUnit-cim:GeneratingUnit.initialMW", "etx-NuclearGeneratingUnit-etx:GeneratingUnit.highReasonabilityLimit", "etx-NuclearGeneratingUnit-etx:GeneratingUnit.lowReasonabilityLimit", "NUCLEAR");
    pop_gen(&mut gendata, &wg, "etx-WindGeneratingUnit-etx:WindGeneratingUnit", "etx-WindGeneratingUnit-cim:IdentifiedObject.name", "etx-WindGeneratingUnit-etx:PowerSystemResource.teid", "etx-WindGeneratingUnit-cim:GeneratingUnit.initialMW", "etx-WindGeneratingUnit-etx:GeneratingUnit.highReasonabilityLimit", "etx-WindGeneratingUnit-etx:GeneratingUnit.lowReasonabilityLimit", "WIND");
    pop_gen(&mut gendata, &sg, "etx-SolarGeneratingUnit-etx:SolarGeneratingUnit", "etx-SolarGeneratingUnit-cim:IdentifiedObject.name", "etx-SolarGeneratingUnit-etx:PowerSystemResource.teid", "etx-SolarGeneratingUnit-cim:GeneratingUnit.initialMW", "etx-SolarGeneratingUnit-etx:GeneratingUnit.highReasonabilityLimit", "etx-SolarGeneratingUnit-etx:GeneratingUnit.lowReasonabilityLimit", "SOLAR");
    // Gen bus numbers
    for i in 0..gendata.n() {
        if let Some(rs) = t_ce_idx.get(gendata.gr(i, 2)) {
            if let Some(&tr) = rs.first() {
                gendata.s(i, 8, t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                gendata.s(i, 0, format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), gendata.gr(i, 4)));
            }
        }
    }

    log("Building Load Data...".to_string());
    // ── Load Data ──
    let mut loaddata = Table::new();
    for c in ["Connectivity Node","Bus Number","Load ID","NumID","Status","Area","Zone","PL","QL","IP","IQ","YP","YQ","Owner","LDF","Demand Zone","Demand Zone Original","Load Zone","Substation","cim-Dam-MRIDLoad","CIM PL Fixed","CIM QL Fixed","CIM PL Nom","CIM QL Nom","PSSEID","Power System Resource TEID","PSSE Bus Number","PSSE Bus Name"] { loaddata.add_col(c); }
    let mut numid = 1i32;
    let t_cul_idx = t.index(t.c("cim-CustomerLoad-cim:CustomerLoad"));
    for i in 0..cul.n() {
        let r = loaddata.add_row();
        loaddata.s(r, loaddata.c("Connectivity Node"), cul.gr(i, cul.c("cim-CustomerLoad-cim:CustomerLoad")));
        loaddata.s(r, loaddata.c("Load ID"), cul.gr(i, cul.c("cim-CustomerLoad-cim:IdentifiedObject.name")));
        loaddata.s(r, loaddata.c("Status"), "1".to_string());
        let df = cul.gr(i, cul.c("cim-Dam-DistributionFactor"));
        loaddata.s(r, loaddata.c("PL"), if df.is_empty() { "0" } else { df });
        loaddata.s(r, loaddata.c("NumID"), rc(numid.to_string())); numid += 1; if numid > 50 { numid = 1; }
        loaddata.s(r, loaddata.c("QL"), cul.gr(i, cul.c("cim-Dam-MVARDistributionFactor")));
        for c in ["IP","IQ","YP","YQ"] { loaddata.s(r, loaddata.c(c), "0".to_string()); }
        loaddata.s(r, loaddata.c("Substation"), cul.gr(i, cul.c("cim-Dam-SubStation")));
        loaddata.s(r, loaddata.c("cim-Dam-MRIDLoad"), cul.gr(i, cul.c("cim-Dam-MRIDLoad")));
        loaddata.s(r, loaddata.c("CIM PL Fixed"), cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.pfixed")));
        loaddata.s(r, loaddata.c("CIM QL Fixed"), cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.qfixed")));
        loaddata.s(r, loaddata.c("CIM PL Nom"), cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.pnom")));
        loaddata.s(r, loaddata.c("CIM QL Nom"), cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.qnom")));
        loaddata.s(r, loaddata.c("PSSEID"), cul.gr(i, cul.c("cim-CustomerLoad-etx:Equipment.psseid")));
        loaddata.s(r, loaddata.c("Power System Resource TEID"), cul.gr(i, cul.c("cim-CustomerLoad-etx:PowerSystemResource.teid")));
    }

    // Load data joins via HashMap
    let mut nc = [0f64; 9]; // NORTHCEN, SOUTHCEN, WEST, COAST, NORTH, FARWEST, SOUTHERN, EAST, FLAT
    for i in 0..loaddata.n() {
        if let Some(rs) = t_cul_idx.get(loaddata.gr(i, loaddata.c("Connectivity Node"))) {
            if let Some(&tr) = rs.first() {
                loaddata.s(i, loaddata.c("Bus Number"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                let noie = t.gr(tr, t.c("NOIE Load Zone ID"));
                if noie.is_empty() {
                    loaddata.s(i, loaddata.c("Area"), t.gr(tr, t.c("Load Zone ID")));
                    loaddata.s(i, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                } else {
                    loaddata.s(i, loaddata.c("Area"), noie);
                    loaddata.s(i, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name")));
                }
                let dzv = t.gr(tr, t.c("Demand Zone ID"));
                if dzv.is_empty() {
                    loaddata.s(i, loaddata.c("Zone"), "1".to_string());
                } else {
                    loaddata.s(i, loaddata.c("Zone"), dzv);
                    let dz = t.gr(tr, t.c("cim-SubLoadArea-Dummy SL"));
                    loaddata.s(i, loaddata.c("Demand Zone"), dz);
                    loaddata.s(i, loaddata.c("Demand Zone Original"), dz);
                    let pl = parse_f(loaddata.gr(i, loaddata.c("PL")));
                    match loaddata.gr(i, loaddata.c("Demand Zone")) {
                        "NORTHCEN" => nc[0] += pl, "SOUTHCEN" => nc[1] += pl, "WEST" => nc[2] += pl,
                        "COAST" => nc[3] += pl, "NORTH" => nc[4] += pl, "FARWEST" => nc[5] += pl,
                        "SOUTHERN" => nc[6] += pl, "EAST" => nc[7] += pl, _ => {}
                    }
                }
                let hidv = t.gr(tr, t.c("HubID"));
                loaddata.s(i, loaddata.c("Owner"), if hidv.is_empty() { "1" } else { hidv });
                loaddata.s(i, loaddata.c("PSSE Bus Number"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                loaddata.s(i, loaddata.c("PSSE Bus Name"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
            }
        }
    }

    // DM loads not found yet — match via Clean keys
    let dm_clean = dm.c("cim-Dam-MRIDLoad Clean");
    let dm_found = dm.c("DM Load Found");
    let t_ce_clean = t.c("cim-Terminal-cim:Terminal.ConductingEquipment Clean");
    let t_ce_clean_idx = t.index(t_ce_clean);
    for i in 0..dm.n() {
        if dm.gr(i, dm_found) == "1" { continue; }
        if let Some(rs) = t_ce_clean_idx.get(dm.gr(i, dm_clean)) {
            if let Some(&tr) = rs.first() {
                dm.s(i, dm_found, "1".to_string());
                let r = loaddata.add_row();
                loaddata.s(r, loaddata.c("Connectivity Node"), dm.gr(i, dm.c("cim-Dam-MRIDLoad")));
                loaddata.s(r, loaddata.c("Load ID"), dm.gr(i, dm.c("cim-Dam-LoadID")));
                loaddata.s(r, loaddata.c("Status"), "1".to_string());
                let df = dm.gr(i, dm.c("cim-Dam-DistributionFactor"));
                loaddata.s(r, loaddata.c("PL"), if df.is_empty() { "0" } else { df });
                loaddata.s(r, loaddata.c("NumID"), rc(numid.to_string())); numid += 1; if numid > 50 { numid = 1; }
                loaddata.s(r, loaddata.c("QL"), dm.gr(i, dm.c("cim-Dam-MVARDistributionFactor")));
                for c in ["IP","IQ","YP","YQ"] { loaddata.s(r, loaddata.c(c), "0".to_string()); }
                loaddata.s(r, loaddata.c("Substation"), dm.gr(i, dm.c("cim-Dam-SubStation")));
                loaddata.s(r, loaddata.c("cim-Dam-MRIDLoad"), dm.gr(i, dm.c("cim-Dam-MRIDLoad")));
                loaddata.s(r, loaddata.c("Bus Number"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                let noie = t.gr(tr, t.c("NOIE Load Zone ID"));
                if noie.is_empty() {
                    loaddata.s(r, loaddata.c("Area"), t.gr(tr, t.c("Load Zone ID")));
                    loaddata.s(r, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                } else {
                    loaddata.s(r, loaddata.c("Area"), noie);
                    loaddata.s(r, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name")));
                }
                loaddata.s(r, loaddata.c("Zone"), rc(zidflat.to_string()));
                loaddata.s(r, loaddata.c("Demand Zone"), "FLAT".to_string());
                nc[8] += parse_f(loaddata.gr(r, loaddata.c("PL")));
                let hidv = t.gr(tr, t.c("HubID"));
                loaddata.s(r, loaddata.c("Owner"), if hidv.is_empty() { "1" } else { hidv });
                loaddata.s(r, loaddata.c("PSSE Bus Number"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                loaddata.s(r, loaddata.c("PSSE Bus Name"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
            }
        }
    }

    // DM loads still not found — match via substation name
    let dm_sub = dm.c("cim-Dam-Substation");
    let t_ss_name = t.c("cim-Substation-cim:IdentifiedObject.name");
    let t_ss_idx = t.index(t_ss_name);
    for i in 0..dm.n() {
        if dm.gr(i, dm_found) == "1" { continue; }
        let sub_key = dm.gr(i, dm_sub).to_string();
        if let Some(rs) = t_ss_idx.get(sub_key.as_str()) {
            if let Some(&tr) = rs.first() {
                dm.s(i, dm_found, "1".to_string());
                let r = loaddata.add_row();
                loaddata.s(r, loaddata.c("Connectivity Node"), dm.gr(i, dm.c("cim-Dam-MRIDLoad")));
                loaddata.s(r, loaddata.c("Load ID"), dm.gr(i, dm.c("cim-Dam-LoadID")));
                loaddata.s(r, loaddata.c("Status"), "1".to_string());
                let df = dm.gr(i, dm.c("cim-Dam-DistributionFactor"));
                loaddata.s(r, loaddata.c("PL"), if df.is_empty() { "0" } else { df });
                loaddata.s(r, loaddata.c("NumID"), rc(numid.to_string())); numid += 1; if numid > 50 { numid = 1; }
                loaddata.s(r, loaddata.c("QL"), dm.gr(i, dm.c("cim-Dam-MVARDistributionFactor")));
                for c in ["IP","IQ","YP","YQ"] { loaddata.s(r, loaddata.c(c), "0".to_string()); }
                loaddata.s(r, loaddata.c("Substation"), dm.gr(i, dm.c("cim-Dam-SubStation")));
                loaddata.s(r, loaddata.c("cim-Dam-MRIDLoad"), dm.gr(i, dm.c("cim-Dam-MRIDLoad")));
                loaddata.s(r, loaddata.c("Bus Number"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                let noie = t.gr(tr, t.c("NOIE Load Zone ID"));
                if noie.is_empty() {
                    loaddata.s(r, loaddata.c("Area"), t.gr(tr, t.c("Load Zone ID")));
                    loaddata.s(r, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                } else {
                    loaddata.s(r, loaddata.c("Area"), noie);
                    loaddata.s(r, loaddata.c("Load Zone"), t.gr(tr, t.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name")));
                }
                loaddata.s(r, loaddata.c("Zone"), rc(zidflat.to_string()));
                loaddata.s(r, loaddata.c("Demand Zone"), "FLAT".to_string());
                nc[8] += parse_f(loaddata.gr(r, loaddata.c("PL")));
                let hidv = t.gr(tr, t.c("HubID"));
                loaddata.s(r, loaddata.c("Owner"), if hidv.is_empty() { "1" } else { hidv });
                loaddata.s(r, loaddata.c("PSSE Bus Number"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                loaddata.s(r, loaddata.c("PSSE Bus Name"), t.gr(tr, t.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
            }
        }
    }

    // Write initial load data
    loaddata.write_csv(&out.join("CIM Load Data Raw.csv")).ok();

    // Total load per demand zone
    let sl_dummy = sl.c("cim-SubLoadArea-Dummy SL");
    let tl = sl.c("Total Load");
    for i in 0..sl.n() {
        let v = match sl.g(i, sl_dummy).as_ref() {
            "NORTHCEN" => nc[0], "SOUTHCEN" => nc[1], "WEST" => nc[2], "COAST" => nc[3],
            "NORTH" => nc[4], "FARWEST" => nc[5], "SOUTHERN" => nc[6], "EAST" => nc[7],
            "FLAT" => nc[8], _ => continue,
        };
        sl.s(i, tl, rc(v.to_string()));
    }

    // LDF calculation
    for i in 0..loaddata.n() {
        let dz_name = loaddata.gr(i, loaddata.c("Demand Zone"));
        let total = match dz_name.as_ref() {
            "NORTHCEN" => nc[0], "SOUTHCEN" => nc[1], "WEST" => nc[2], "COAST" => nc[3],
            "NORTH" => nc[4], "FARWEST" => nc[5], "SOUTHERN" => nc[6], "EAST" => nc[7],
            "FLAT" => nc[8], _ => 0.0,
        };
        if total != 0.0 { loaddata.s(i, loaddata.c("LDF"), (parse_f(loaddata.gr(i, loaddata.c("PL"))) / total * 100.0).to_string()); }
    }

    log("Building Line Data...".to_string());
    // ── Line Data ──
    let mut linedata = Table::new();
    for c in ["Identifier","Type","ID Number","Line Name","From Bus","To Bus","From Bus Voltage","To Bus Voltage","CKT","R Ohms","R PU","X Ohms","X PU","B","B PU","Rate A","Rate B","Rate C","GIBI","GIBJ","Status","Met","Length","O1","F1","O2","F2","O3","F3","O4","F4","From Substation","From Full Substation","To Substation","To Full Substation"] { linedata.add_col(c); }
    for i in 0..ac.n() {
        let r = linedata.add_row();
        linedata.s(r, linedata.c("Identifier"), ac.gr(i, ac.c("cim-ACLineSegment-cim:ACLineSegment")));
        linedata.s(r, linedata.c("Type"), "AClinesegment".to_string());
        linedata.s(r, linedata.c("ID Number"), ac.gr(i, ac.c("cim-ACLineSegment-etx:PowerSystemResource.teid")));
        linedata.s(r, linedata.c("R Ohms"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.r")));
        linedata.s(r, linedata.c("X Ohms"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.x")));
        linedata.s(r, linedata.c("B"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.bch")));
        linedata.s(r, linedata.c("Length"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.length")));
        for c in ["GIBI","GIBJ","Status","Met"] { linedata.s(r, linedata.c(c), if c == "Status" || c == "Met" { "1" } else { "0" }.to_string()); }
        for c in ["O1","F1"] { linedata.s(r, linedata.c(c), "1".to_string()); }
        for c in ["F2","F3","F4"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        for c in ["O2","O3","O4"] { linedata.s(r, linedata.c(c), "1".to_string()); }
    }
    log("Line terminals + ratings...".to_string());
    // Line terminals + ratings
    let al_ac_idx = al.index(al.c("cim-ACLineSegment-cim:ACLineSegment"));
    for i in 0..linedata.n() {
        let id = linedata.gr(i, linedata.c("Identifier")).to_string();
        if let Some(rs) = t_ce_idx.get(id.as_str()) {
            let mut flag = 1;
            for &tr in rs {
                if flag == 1 {
                    linedata.s(i, linedata.c("From Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(i, linedata.c("From Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    linedata.s(i, linedata.c("From Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(i, linedata.c("From Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    flag = 2;
                } else {
                    linedata.s(i, linedata.c("To Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(i, linedata.c("To Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    linedata.s(i, linedata.c("To Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(i, linedata.c("To Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    break;
                }
            }
        }
        // Ratings from AL
        if let Some(ars) = al_ac_idx.get(id.as_str()) {
            for &ar in ars {
                let rn = al.gr(ar, al.c("etx-Rating-cim:IdentifiedObject.name"));
                if matches!(rn.as_ref(), "staticRating" | "Static" | "StaticRating") {
                    let an = al.gr(ar, al.c("cim-AnalogLimit-cim:IdentifiedObject.name"));
                    let val = al.gr(ar, al.c("cim-AnalogLimit-cim:AnalogLimit.value"));
                    match an.as_ref() { "fifteenminuteRating" => linedata.s(i, linedata.c("Rate C"), val), "twoHourRating" => linedata.s(i, linedata.c("Rate B"), val), "normalRating" => linedata.s(i, linedata.c("Rate A"), val), _ => {} }
                }
                let ln = al.gr(ar, al.c("cim-Line-cim:IdentifiedObject.name"));
                let acn = al.gr(ar, al.c("cim-ACLineSegment-cim:IdentifiedObject.name"));
                if !ln.is_empty() || !acn.is_empty() { linedata.s(i, linedata.c("Line Name"), format!("{}{}", ln, acn)); linedata.s(i, linedata.c("CKT"), "1".to_string()); }
            }
        }
        let fv = parse_f(linedata.gr(i, linedata.c("From Bus Voltage")));
        let ro = parse_f(linedata.gr(i, linedata.c("R Ohms")));
        let xo = linedata.gr(i, linedata.c("X Ohms")).to_string();
        let bv = parse_f(linedata.gr(i, linedata.c("B")));
        if fv != 0.0 {
            linedata.s(i, linedata.c("R PU"), (ro * 100.0 / (fv * fv)).to_string());
            let xval = parse_f(&xo);
            linedata.s(i, linedata.c("X PU"), if xval == 0.0 { "0.00001".to_string() } else { (xval * 100.0 / (fv * fv)).to_string() });
            linedata.s(i, linedata.c("B PU"), (bv * fv * fv / 100.0).to_string());
        }
    }

    log("Building Series Compensator lines...".to_string());
    // ── Series Compensator lines ──
    for i in 0..sec.n() {
        let r = linedata.add_row();
        linedata.s(r, linedata.c("Identifier"), sec.gr(i, sec.c("cim-SeriesCompensator-cim:SeriesCompensator")));
        linedata.s(r, linedata.c("Type"), "Series Compensator".to_string());
        linedata.s(r, linedata.c("ID Number"), sec.gr(i, sec.c("cim-SeriesCompensator-etx:PowerSystemResource.teid")));
        linedata.s(r, linedata.c("R Ohms"), sec.gr(i, sec.c("cim-SeriesCompensator-cim:SeriesCompensator.r")));
        linedata.s(r, linedata.c("X Ohms"), sec.gr(i, sec.c("cim-SeriesCompensator-cim:SeriesCompensator.x")));
        for c in ["B","B PU","Length","GIBI","GIBJ"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        linedata.s(r, linedata.c("Status"), "1".to_string()); linedata.s(r, linedata.c("Met"), "1".to_string());
        for c in ["O1","F1","O2","O3","O4"] { linedata.s(r, linedata.c(c), "1".to_string()); }
        for c in ["F2","F3","F4"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        for c in ["Rate A","Rate B","Rate C"] { linedata.s(r, linedata.c(c), "9999".to_string()); }
        let id = linedata.gr(r, linedata.c("Identifier")).to_string();
        if let Some(rs) = t_ce_idx.get(id.as_str()) {
            let mut flag = 1;
            for &tr in rs {
                if flag == 1 {
                    linedata.s(r, linedata.c("Line Name"), format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), sec.gr(i, sec.c("cim-SeriesCompensator-etx:PowerSystemResource.teid"))));
                    linedata.s(r, linedata.c("CKT"), "1".to_string());
                    linedata.s(r, linedata.c("From Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("From Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    linedata.s(r, linedata.c("To Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("To Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    flag = 2;
                } else {
                    linedata.s(r, linedata.c("To Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("To Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    break;
                }
            }
        }
        let fv = parse_f(linedata.gr(r, linedata.c("From Bus Voltage")));
        if fv != 0.0 {
            let ro = parse_f(linedata.gr(r, linedata.c("R Ohms")));
            linedata.s(r, linedata.c("R PU"), (ro * 100.0 / (fv * fv)).to_string());
            let xval = parse_f(linedata.gr(r, linedata.c("X Ohms")));
            linedata.s(r, linedata.c("X PU"), if xval == 0.0 { "0.00001".to_string() } else { (xval * 100.0 / (fv * fv)).to_string() });
        }
    }

    log("Building DC Tie...".to_string());
    // ── DC Tie connectivity node ──
    let dc_cn = dc.add_col("Connectivity Node");
    let dc_ce = dc.c("etx-DCTie-etx:DCTie.EnergyConsumer");
    for i in 0..dc.n() {
        if let Some(rs) = t_ce_idx.get(dc.gr(i, dc_ce)) { if let Some(&tr) = rs.first() { dc.s(i, dc_cn, t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid"))); } }
    }

    log("Building Breaker lines...".to_string());
    // ── Breaker lines ──
    for i in 0..b.n() {
        let r = linedata.add_row();
        linedata.s(r, linedata.c("Identifier"), b.gr(i, b.c("cim-Breaker-cim:Breaker")));
        linedata.s(r, linedata.c("Type"), "Breaker".to_string());
        linedata.s(r, linedata.c("ID Number"), b.gr(i, b.c("cim-Breaker-etx:PowerSystemResource.teid")));
        for c in ["R Ohms","R PU","X Ohms","B","B PU","Length","GIBI","GIBJ"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        linedata.s(r, linedata.c("X PU"), "0.00001".to_string());
        let no = b.gr(i, b.c("cim-Breaker-cim:Switch.normalOpen"));
        linedata.s(r, linedata.c("Status"), if no.eq_ignore_ascii_case("true") { "0" } else { "1" }.to_string());
        linedata.s(r, linedata.c("Met"), "1".to_string());
        for c in ["O1","F1","O2","O3","O4"] { linedata.s(r, linedata.c(c), "1".to_string()); }
        for c in ["F2","F3","F4"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        for c in ["Rate A","Rate B","Rate C"] { linedata.s(r, linedata.c(c), "9999".to_string()); }
        let id = linedata.gr(r, linedata.c("Identifier")).to_string();
        if let Some(rs) = t_ce_idx.get(id.as_str()) {
            let mut flag = 1;
            for &tr in rs {
                if flag == 1 {
                    linedata.s(r, linedata.c("Line Name"), format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), b.gr(i, b.c("cim-Breaker-cim:IdentifiedObject.name"))));
                    linedata.s(r, linedata.c("CKT"), "1".to_string());
                    linedata.s(r, linedata.c("From Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("From Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    linedata.s(r, linedata.c("To Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("To Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    flag = 2;
                } else {
                    linedata.s(r, linedata.c("To Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("To Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    break;
                }
            }
        }
    }

    log("Building Disconnector lines...".to_string());
    // ── Disconnector lines ──
    for i in 0..d.n() {
        let r = linedata.add_row();
        linedata.s(r, linedata.c("Identifier"), d.gr(i, d.c("cim-Disconnector-cim:Disconnector")));
        linedata.s(r, linedata.c("Type"), "Disconnector".to_string());
        linedata.s(r, linedata.c("ID Number"), d.gr(i, d.c("cim-Disconnector-etx:PowerSystemResource.teid")));
        for c in ["R Ohms","R PU","X Ohms","B","B PU","Length","GIBI","GIBJ"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        linedata.s(r, linedata.c("X PU"), "0.00001".to_string());
        let no = d.gr(i, d.c("cim-Disconnector-cim:Switch.normalOpen"));
        linedata.s(r, linedata.c("Status"), if no.eq_ignore_ascii_case("true") { "0" } else { "1" }.to_string());
        linedata.s(r, linedata.c("Met"), "1".to_string());
        for c in ["O1","F1","O2","O3","O4"] { linedata.s(r, linedata.c(c), "1".to_string()); }
        for c in ["F2","F3","F4"] { linedata.s(r, linedata.c(c), "0".to_string()); }
        for c in ["Rate A","Rate B","Rate C"] { linedata.s(r, linedata.c(c), "9999".to_string()); }
        let id = linedata.gr(r, linedata.c("Identifier")).to_string();
        if let Some(rs) = t_ce_idx.get(id.as_str()) {
            let mut flag = 1;
            for &tr in rs {
                if flag == 1 {
                    linedata.s(r, linedata.c("Line Name"), format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), d.gr(i, d.c("cim-Disconnector-cim:IdentifiedObject.name"))));
                    linedata.s(r, linedata.c("CKT"), "1".to_string());
                    linedata.s(r, linedata.c("From Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("From Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("From Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    linedata.s(r, linedata.c("To Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                    linedata.s(r, linedata.c("To Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
                    flag = 2;
                } else {
                    linedata.s(r, linedata.c("To Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                    linedata.s(r, linedata.c("To Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    break;
                }
            }
        }
    }

    log("Building Transformer Data...".to_string());
    // ── Transformer Data ──
    let mut xmrdata = Table::new();
    for c in ["Identifier","Type","ID Number","Power Transformer ID","Line Name","From Bus","To Bus","Tertiary Bus","Primary Winding","Secondary Winding","Primary Winding ID","Secondary Winding ID","Primary Winding Voltage","Secondary Winding Voltage","Primary Winding X","Secondary Winding X","Primary Winding R","Secondary Winding R","From Bus Voltage","To Bus Voltage","CKT","CW","CZ","CM","Mag1","Mag2","NMETR","Name","STAT","O1","F1","O2","F2","O3","F3","O4","F4","R1-2","X1-2","SBASE1-2","WINDV1","NOMV1","ANG1","RATE A","RATE B","RATE C","Substation","Full Substation"] { xmrdata.add_col(c); }
    let tw_pt_idx = tw.index(tw.c("cim-TransformerWinding-cim:TransformerWinding.MemberOf_PowerTransformer"));
    log("Building al1_pt_idx...".to_string());
    let al1_pt_idx = al1.index(al1.c("cim-TransformerWinding-cim:TransformerWinding.MemberOf_PowerTransformer"));
    log("al1_pt_idx done.".to_string());
    for i in 0..pt.n() {
        let r = xmrdata.add_row();
        let pt_id = pt.gr(i, pt.c("cim-PowerTransformer-cim:PowerTransformer"));
        xmrdata.s(r, xmrdata.c("Identifier"), pt_id);
        xmrdata.s(r, xmrdata.c("Type"), "PowerTransformer".to_string());
        xmrdata.s(r, xmrdata.c("ID Number"), pt.gr(i, pt.c("cim-PowerTransformer-etx:PowerSystemResource.teid")));
        xmrdata.s(r, xmrdata.c("Power Transformer ID"), pt.gr(i, pt.c("cim-PowerTransformer-cim:IdentifiedObject.name")));
        for c in ["CKT","CW","CZ","CM","Name","STAT","O1","F1","O2","O3","O4"] { xmrdata.s(r, xmrdata.c(c), "1".to_string()); }
        for c in ["F2","F3","F4","Mag1","Mag2","NOMV1","ANG1"] { xmrdata.s(r, xmrdata.c(c), "0".to_string()); }
        xmrdata.s(r, xmrdata.c("NMETR"), "1".to_string());
        xmrdata.s(r, xmrdata.c("WINDV1"), "1".to_string());
        // Windings
        if let Some(wrs) = tw_pt_idx.get(pt_id) {
            let mut wf = 1;
            for &wr in wrs {
                if wf == 1 {
                    xmrdata.s(r, xmrdata.c("Primary Winding"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding")));
                    xmrdata.s(r, xmrdata.c("Primary Winding ID"), tw.gr(wr, tw.c("cim-TransformerWinding-etx:PowerSystemResource.teid")));
                    xmrdata.s(r, xmrdata.c("Primary Winding Voltage"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.ratedKV")));
                    xmrdata.s(r, xmrdata.c("Primary Winding X"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.x")));
                    xmrdata.s(r, xmrdata.c("Primary Winding R"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.r")));
                    wf = 2;
                } else {
                    xmrdata.s(r, xmrdata.c("Secondary Winding"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding")));
                    xmrdata.s(r, xmrdata.c("Secondary Winding ID"), tw.gr(wr, tw.c("cim-TransformerWinding-etx:PowerSystemResource.teid")));
                    xmrdata.s(r, xmrdata.c("Secondary Winding Voltage"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.ratedKV")));
                    xmrdata.s(r, xmrdata.c("Secondary Winding X"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.x")));
                    xmrdata.s(r, xmrdata.c("Secondary Winding R"), tw.gr(wr, tw.c("cim-TransformerWinding-cim:TransformerWinding.r")));
                }
            }
        }
        let rr1 = parse_f(xmrdata.gr(r, xmrdata.c("Primary Winding R")));
        let rr2 = parse_f(xmrdata.gr(r, xmrdata.c("Secondary Winding R")));
        let v1 = parse_f(xmrdata.gr(r, xmrdata.c("Primary Winding Voltage")));
        let v2 = parse_f(xmrdata.gr(r, xmrdata.c("Secondary Winding Voltage")));
        let rx1 = parse_f(xmrdata.gr(r, xmrdata.c("Primary Winding X")));
        let rx2 = parse_f(xmrdata.gr(r, xmrdata.c("Secondary Winding X")));
        let r1_2 = if v1 != 0.0 && v2 != 0.0 { (rr1 / (v1 * v1) + rr2 / (v2 * v2)) * 100.0 } else { 0.0 };
        let x1_2 = if v1 != 0.0 && v2 != 0.0 { (rx1 / (v1 * v1) + rx2 / (v2 * v2)) * 100.0 } else { 0.0 };
        xmrdata.s(r, xmrdata.c("R1-2"), rc(r1_2.to_string()));
        xmrdata.s(r, xmrdata.c("X1-2"), rc(x1_2.to_string()));
        // Bus numbers — O(1) HashMap lookup instead of O(N) linear scan
        let pw = xmrdata.gr(r, xmrdata.c("Primary Winding"));
        let sw = xmrdata.gr(r, xmrdata.c("Secondary Winding")).to_string();
        if let Some(rs) = t_ce_idx.get(pw) {
            if let Some(&tr) = rs.first() {
                xmrdata.s(r, xmrdata.c("From Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                xmrdata.s(r, xmrdata.c("From Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                xmrdata.s(r, xmrdata.c("Line Name"), format!("{}_{}", t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")), xmrdata.gr(r, xmrdata.c("Power Transformer ID"))));
                xmrdata.s(r, xmrdata.c("Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.name")));
                xmrdata.s(r, xmrdata.c("Full Substation"), t.gr(tr, t.c("cim-Substation-cim:IdentifiedObject.description")));
            }
        }
        if let Some(rs) = t_ce_idx.get(sw.as_str()) {
            if let Some(&tr) = rs.first() {
                xmrdata.s(r, xmrdata.c("To Bus"), t.gr(tr, t.c("cim-ConnectivityNode-etx:ConnectivityNode.teid")));
                xmrdata.s(r, xmrdata.c("To Bus Voltage"), t.gr(tr, t.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
            }
        }
        // Ratings from AL1
        if let Some(ars) = al1_pt_idx.get(pt_id) {
            for &ar in ars {
                let rn = al1.gr(ar, al1.c("etx-Rating-cim:IdentifiedObject.name"));
                if matches!(rn.as_ref(), "staticRating" | "Static" | "StaticRating") {
                    let an = al1.gr(ar, al1.c("cim-AnalogLimit-cim:IdentifiedObject.name"));
                    let val = al1.gr(ar, al1.c("cim-AnalogLimit-cim:AnalogLimit.value"));
                    match an.as_ref() { "fifteenminuteRating" => xmrdata.s(r, xmrdata.c("Rate C"), val), "twoHourRating" => xmrdata.s(r, xmrdata.c("Rate B"), val), "normalRating" => xmrdata.s(r, xmrdata.c("Rate A"), val), _ => {} }
                }
            }
        }
    }

    // ── Bus IDE (generator bus detection) ──
    let gen_buses: HashSet<String> = (0..gendata.n()).map(|i| gendata.gr(i, gendata.c("Bus Number")).to_string()).filter(|s| !s.is_empty()).collect();
    let ide_col = busdata.c("IDE");
    let bn_col = busdata.c("Bus Number");
    let bname_col = busdata.c("Bus Name");
    for i in 0..busdata.n() {
        if gen_buses.contains(busdata.gr(i, bn_col)) { busdata.s(i, ide_col, "2".to_string()); }
        if busdata.gr(i, bname_col) == "'MARTLAKE_3G'" { busdata.s(i, ide_col, "3".to_string()); }
    }

    // ── CKT numbering ──
    let mut ck = 0;
    for i in 0..linedata.n() { if ck >= 99 { ck = 0; } ck += 1; linedata.s(i, linedata.c("CKT"), rc(ck.to_string())); }
    let mut ck = 0;
    for i in 0..xmrdata.n() { if ck >= 99 { ck = 0; } ck += 1; xmrdata.s(i, xmrdata.c("CKT"), rc(ck.to_string())); }

    // ── Copies before disconnector elimination ──
    log("Cloning tables for disconnector-level output…".to_string());
    let linedatacopy = linedata.clone_t();
    let busdatacopy = busdata.clone_t();
    let xmrdatacopy = xmrdata.clone_t();
    let loaddatacopy = loaddata.clone_t();
    let gendatacopy = gendata.clone_t();

    // ── Disconnector elimination ── (HashMap-indexed: O(N+K) instead of O(N*M))
    log(format!("Disconnector elimination… linedata={} xmrdata={} loaddata={} gendata={}", linedata.n(), xmrdata.n(), loaddata.n(), gendata.n()));
    let ld_type = linedata.c("Type"); let ld_fb = linedata.c("From Bus"); let ld_tb = linedata.c("To Bus");
    let ld_fbv = linedata.c("From Bus Voltage"); let ld_tbv = linedata.c("To Bus Voltage");
    let ld_fs = linedata.c("From Substation"); let ld_ts = linedata.c("To Substation");
    let ld_ffs = linedata.c("From Full Substation"); let ld_tfs = linedata.c("To Full Substation");
    let xm_fb = xmrdata.c("From Bus"); let xm_tb = xmrdata.c("To Bus");
    let ld_bn = loaddata.c("Bus Number"); let gd_bn = gendata.c("Bus Number");

    // Build bus->rows indexes for O(1) lookup instead of O(M) scan
    let mut ld_idx: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for j in 0..linedata.n() {
        let fb = linedata.gr(j, ld_fb).to_string();
        if !fb.is_empty() { ld_idx.entry(fb).or_default().push((j, ld_fb)); }
        let tb = linedata.gr(j, ld_tb).to_string();
        if !tb.is_empty() { ld_idx.entry(tb).or_default().push((j, ld_tb)); }
    }
    let mut xm_idx: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for j in 0..xmrdata.n() {
        let fb = xmrdata.gr(j, xm_fb).to_string();
        if !fb.is_empty() { xm_idx.entry(fb).or_default().push((j, xm_fb)); }
        let tb = xmrdata.gr(j, xm_tb).to_string();
        if !tb.is_empty() { xm_idx.entry(tb).or_default().push((j, xm_tb)); }
    }
    let mut load_idx: HashMap<String, Vec<usize>> = HashMap::new();
    for j in 0..loaddata.n() {
        let bn = loaddata.gr(j, ld_bn).to_string();
        if !bn.is_empty() { load_idx.entry(bn).or_default().push(j); }
    }
    let mut gen_idx: HashMap<String, Vec<usize>> = HashMap::new();
    for j in 0..gendata.n() {
        let bn = gendata.gr(j, gd_bn).to_string();
        if !bn.is_empty() { gen_idx.entry(bn).or_default().push(j); }
    }
    log("Indexes built.".to_string());

    // Discard mapping with reverse map for O(1) chaining
    let mut discard_map: HashMap<String, String> = HashMap::new();
    let mut reverse_map: HashMap<String, Vec<String>> = HashMap::new();

    for i in 0..linedata.n() {
        if linedata.gr(i, ld_type) != "Disconnector" { continue; }
        let r2 = linedata.gr(i, ld_fb).to_string();
        let r3 = linedata.gr(i, ld_tb).to_string();
        if r2 == r3 || r2.is_empty() || r3.is_empty() { continue; }

        // Update discard chaining: if any previous discard pointed to r2, retarget to r3
        if let Some(sources) = reverse_map.remove(&r2) {
            for src in &sources {
                discard_map.insert(src.clone(), r3.clone());
                reverse_map.entry(r3.clone()).or_default().push(src.clone());
            }
        }
        discard_map.insert(r2.clone(), r3.clone());
        reverse_map.entry(r3.clone()).or_default().push(r2.clone());

        // Pre-create Rc<str> values
        let r3_rc = rc(&r3);
        let r4_rc = rc(linedata.gr(i, ld_tbv));
        let r5_rc = rc(linedata.gr(i, ld_ts));
        let r6_rc = rc(linedata.gr(i, ld_tfs));

        // Update linedata rows that reference r2 — O(K) not O(M)
        if let Some(entries) = ld_idx.remove(&r2) {
            for &(j, col) in &entries {
                if col == ld_fb {
                    linedata.s_rc(j, ld_fb, r3_rc.clone()); linedata.s_rc(j, ld_fbv, r4_rc.clone());
                    linedata.s_rc(j, ld_fs, r5_rc.clone()); linedata.s_rc(j, ld_ffs, r6_rc.clone());
                } else {
                    linedata.s_rc(j, ld_tb, r3_rc.clone()); linedata.s_rc(j, ld_tbv, r4_rc.clone());
                    linedata.s_rc(j, ld_ts, r5_rc.clone()); linedata.s_rc(j, ld_tfs, r6_rc.clone());
                }
            }
            ld_idx.entry(r3.clone()).or_default().extend(entries);
        }
        // Update xmrdata rows
        if let Some(entries) = xm_idx.remove(&r2) {
            for &(j, col) in &entries {
                if col == xm_fb { xmrdata.s_rc(j, xm_fb, r3_rc.clone()); }
                else { xmrdata.s_rc(j, xm_tb, r3_rc.clone()); }
            }
            xm_idx.entry(r3.clone()).or_default().extend(entries);
        }
        // Update loaddata rows
        if let Some(entries) = load_idx.remove(&r2) {
            for &j in &entries { loaddata.s_rc(j, ld_bn, r3_rc.clone()); }
            load_idx.entry(r3.clone()).or_default().extend(entries);
        }
        // Update gendata rows
        if let Some(entries) = gen_idx.remove(&r2) {
            for &j in &entries { gendata.s_rc(j, gd_bn, r3_rc.clone()); }
            gen_idx.entry(r3.clone()).or_default().extend(entries);
        }
    }
    log(format!("Disconnector elimination done. {} buses discarded.", discard_map.len()));
    // Delete discarded buses
    let discard_set: HashSet<String> = discard_map.keys().cloned().collect();
    busdata.rows.retain(|row| { let bn = row.get(bn_col).map(|v| v.as_ref()).unwrap_or(""); !discard_set.contains(bn) });
    // Update resource node bus numbers — single pass
    for i in 0..rndata.n() {
        let bn = rndata.gr(i, 3).to_string();
        if let Some(new) = discard_map.get(&bn) { rndata.s(i, 3, new.as_str()); }
    }

    log(format!("Writing output files… busdata={} loaddata={} linedata={} gendata={} xmrdata={}", busdata.n(), loaddata.n(), linedata.n(), gendata.n(), xmrdata.n()));

    // ── Hub Data ──
    log("Building hub data…".to_string());
    let mut hubdata = Table::new();
    for c in ["Bus Number","Owner","Substation","Hub","Substation Count","Hub Count","Factor"] { hubdata.add_col(c); }
    for i in 0..busdata.n() {
        let owner = busdata.g(i, busdata.c("Owner"));
        if owner != "1" && !owner.is_empty() {
            let r = hubdata.add_row();
            hubdata.s(r, 0, busdata.g(i, busdata.c("Bus Number")));
            hubdata.s(r, 1, owner);
            hubdata.s(r, 2, busdata.g(i, busdata.c("Substation")));
            hubdata.s(r, 3, busdata.g(i, busdata.c("Hub")));
        }
    }
    // Substation counts
    let mut ss_counts: HashMap<String, (String, i64)> = HashMap::new();
    for i in 0..hubdata.n() {
        let ss = hubdata.g(i, 2);
        let entry = ss_counts.entry(ss).or_insert_with(|| (hubdata.g(i, 3), 0));
        entry.1 += 1;
    }
    let mut hub_counts: HashMap<String, i64> = HashMap::new();
    for i in 0..sh.n() {
        let hn = sh.g(i, sh.c("etx-SettlementHUB-cim:IdentifiedObject.name"));
        if hn != "1" { *hub_counts.entry(hn).or_insert(0) += 0; } // init to 0
    }
    for i in 0..hubdata.n() { *hub_counts.entry(hubdata.g(i, 3)).or_insert(0) += 1; }
    for i in 0..hubdata.n() {
        let ss = hubdata.g(i, 2);
        let sc = ss_counts.get(ss.as_str()).map_or(1, |e| e.1);
        let hc = hub_counts.get(hubdata.gr(i, 3)).map_or(1, |&c| c);
        hubdata.s(i, 4, rc(sc.to_string()));
        hubdata.s(i, 5, rc(hc.to_string()));
        hubdata.s(i, 6, (1.0 / (sc as f64 * hc as f64)).to_string());
    }

    // ── ERCOT Hub Map ──
    let mut ercothubmap = Table::new();
    for c in ["COMM_NODE", "Bus Number", "Count"] { ercothubmap.add_col(c); }
    for i in 0..hubdata.n() {
        let r = ercothubmap.add_row();
        ercothubmap.s(r, 0, hubdata.g(i, 3));
        ercothubmap.s(r, 1, hubdata.g(i, 0));
        ercothubmap.s(r, 2, (1.0 / parse_f(&hubdata.g(i, 6))).to_string());
    }
    for i in 0..rndata.n() {
        let r = ercothubmap.add_row();
        ercothubmap.s(r, 0, rndata.g(i, 2));
        ercothubmap.s(r, 1, rndata.g(i, 3));
        ercothubmap.s(r, 2, "1".to_string());
    }

    // ── ERCOT LZ Map ──
    let mut ercotlzmap = Table::new();
    for c in ["COMM_NODE", "Bus Number", "Count"] { ercotlzmap.add_col(c); }
    for i in 0..loaddata.n() {
        let r = ercotlzmap.add_row();
        ercotlzmap.s(r, 0, loaddata.gr(i, loaddata.c("Load Zone")));
        ercotlzmap.s(r, 1, loaddata.gr(i, loaddata.c("Bus Number")));
        ercotlzmap.s(r, 2, "1".to_string());
    }

    // ── Write CSVs ──
    loaddata.write_csv(&out.join("Initial CIM Load Data.csv")).ok();
    loaddata.add_col("Load Name"); loaddata.add_col("Load Status");
    for i in 0..loaddata.n() { loaddata.s(i, loaddata.c("Load Name"), format!("{}_{}", loaddata.g(i, loaddata.c("Substation")), loaddata.g(i, loaddata.c("Load ID")))); }
    loaddata.write_csv(&out.join("Final CIM Load Data.csv")).ok();
    busdata.write_csv(&out.join("Bus Data.csv")).ok();
    ercothubmap.write_csv(&out.join("ERCOT_Hub_Map.csv")).ok();
    ercotlzmap.write_csv(&out.join("ERCOT_LZ_Map.csv")).ok();
    hubdata.write_csv(&out.join("Hub Data.csv")).ok();
    busdatacopy.write_csv(&out.join("Bus Data Copy.csv")).ok();
    // Discard bus data
    let mut dtable = Table::new();
    dtable.add_col("Discarded Bus Number"); dtable.add_col("New Bus Number");
    for (a, b) in &discard_map { let r = dtable.add_row(); dtable.s(r, 0, a.clone()); dtable.s(r, 1, b.clone()); }
    dtable.write_csv(&out.join("Discard Bus Data.csv")).ok();
    dc.write_csv(&out.join("DC Tie.csv")).ok();
    sl.write_csv(&out.join("Demand Zone Load.csv")).ok();
    linedata.write_csv(&out.join("Line Data.csv")).ok();
    rndata.write_csv(&out.join("Resource Node Data.csv")).ok();
    linedatacopy.write_csv(&out.join("Line Data Disconnector Level.csv")).ok();
    shuntdata.write_csv(&out.join("Shunt Data.csv")).ok();
    xmrdata.write_csv(&out.join("Transformer Data.csv")).ok();
    xmrdatacopy.write_csv(&out.join("Transformer Data Disconnector Level.csv")).ok();
    gendata.write_csv(&out.join("Generator Data.csv")).ok();
    gendatacopy.write_csv(&out.join("Generator Data Disconnector.csv")).ok();
    t.write_csv(&out.join("Terminal Data.csv")).ok();

    // ── Write PSSE .raw files ──
    log("Writing .raw files…".to_string());
    write_raw(&out.join("CimNoDisconnector.raw"), &busdata, &loaddata, &gendata, &linedata, &xmrdata, &lz, &no, &sl, &sh, true).ok();
    write_raw(&out.join("Cim.raw"), &busdatacopy, &loaddatacopy, &gendata, &linedatacopy, &xmrdatacopy, &lz, &no, &sl, &sh, false).ok();

    // ── Corrected Contingency processing (from Contingency Test code) ───────
    log("Contingency processing…".to_string());
    // Reads Contingency.csv, joins to Line Data & Transformer Data via HashMap.
    let cont_path = out.join("Contingency.csv");
    if cont_path.exists() {
        if let Ok(ctab) = Table::load(&cont_path, "Contingency") {
            let mut newuplan = Table::new();
            newuplan.add_col("Contingency Name"); newuplan.add_col("Line"); newuplan.add_col("LineID"); newuplan.add_col("Fail");
            let cid = ctab.c("Contingency-Contingency ID");
            let etype = ctab.c("Contingency-Element type");
            let teid = ctab.c("Contingency-Element TEID");
            let mut finalcontig = String::new();
            for i in 0..ctab.n() {
                let conting = ctab.g(i, cid);
                if !conting.is_empty() && conting != "\"" {
                    finalcontig = conting;
                } else {
                    let et = ctab.g(i, etype);
                    if et == "ACLineSegment" || et == "Breaker" {
                        let r = newuplan.add_row();
                        newuplan.s(r, newuplan.c("Contingency Name"), finalcontig.clone());
                        newuplan.s(r, newuplan.c("LineID"), ctab.g(i, teid));
                    }
                }
            }
            // Join to Line Data (on LineID = Line Data-ID Number)
            let ld_idx = linedata.index(linedata.c("ID Number"));
            for i in 0..newuplan.n() {
                let key = newuplan.g(i, newuplan.c("LineID"));
                if let Some(rs) = ld_idx.get(key.as_str()) {
                    for &r in rs {
                        newuplan.s(i, newuplan.c("Line"), linedata.gr(r, linedata.c("Line Name")));
                        if linedata.gr(r, linedata.c("From Bus")) == linedata.gr(r, linedata.c("To Bus")) {
                            newuplan.s(i, newuplan.c("Fail"), "1".to_string());
                        }
                    }
                }
            }
            // Join to Transformer Data (on LineID = Transformer Data-ID Number)
            let td_idx = xmrdata.index(xmrdata.c("ID Number"));
            for i in 0..newuplan.n() {
                let key = newuplan.g(i, newuplan.c("LineID"));
                if let Some(rs) = td_idx.get(key.as_str()) {
                    for &r in rs {
                        newuplan.s(i, newuplan.c("Line"), xmrdata.gr(r, xmrdata.c("Line Name")));
                        if xmrdata.gr(r, xmrdata.c("From Bus")) == xmrdata.gr(r, xmrdata.c("To Bus")) {
                            newuplan.s(i, newuplan.c("Fail"), "1".to_string());
                        }
                    }
                }
            }
            newuplan.write_csv(&out.join("UPLAN Contingency New.csv")).ok();
        }
    }

    log(format!("Model build complete in {:.1}s", start.elapsed().as_secs_f64()));
    progress(Progress::Done);
}

// ── Helper: clean ConductingEquipment column ───────────────────────────────
impl Table {
    fn clean_ce(&mut self) {
        let src = self.c("cim-Terminal-cim:Terminal.ConductingEquipment");
        let dst = self.add_col("cim-Terminal-cim:Terminal.ConductingEquipment Clean");
        for i in 0..self.n() {
            let v = rc(self.gr(i, src).replace("{", "").replace("}", "").trim());
            self.s_rc(i, dst, v);
        }
    }
    fn clean_col(&mut self, src: &str) {
        let sc = self.c(src);
        let dst = self.add_col(&format!("{src} Clean"));
        for i in 0..self.n() {
            let v = rc(self.gr(i, sc).replace("{", "").replace("}", "").trim());
            self.s_rc(i, dst, v);
        }
    }
}

// ── Helper: rework analog limit names ──────────────────────────────────────
fn rework_al(t: &mut Table) {
    let c = t.c("cim-AnalogLimit-cim:IdentifiedObject.name");
    if c == usize::MAX { return; }
    for i in 0..t.n() {
        let m = match t.gr(i, c) {
            "COND" | "NMRL" | "NRML" => Some("normalRating"),
            "EMGY" | "twoHrRating" | "2HR" | "2 HourRating" => Some("twoHourRating"),
            "fifMnRating" | "LDSD" | "15MN" | "15MIN" | "15 Min Rating" => Some("fifteenminuteRating"),
            _ => None,
        };
        if let Some(m) = m { t.s(i, c, rc(m)); }
    }
}

// ── Helper: populate generation data ───────────────────────────────────────
fn pop_gen(g: &mut Table, s: &Table, id_col: &str, name_col: &str, teid_col: &str, mw_col: &str, hrl_col: &str, lrl_col: &str, gtype: &str) {
    for i in 0..s.n() {
        let r = g.add_row();
        g.s(r, g.c("Gen ID"), s.gr(i, s.c(id_col)));
        g.s(r, g.c("Generator Type"), rc(gtype));
        g.s(r, g.c("Machine ID"), s.gr(i, s.c("cim-SynchronousMachine-cim:SynchronousMachine")));
        g.s(r, g.c("Identifier Object Name"), s.gr(i, s.c(name_col)));
        g.s(r, g.c("Machine Identifier Object Name"), s.gr(i, s.c("cim-SynchronousMachine-cim:IdentifiedObject.name")));
        g.s(r, g.c("TEID"), s.gr(i, s.c(teid_col)));
        g.s(r, g.c("Machine TEID"), s.gr(i, s.c("cim-SynchronousMachine-etx:PowerSystemResource.teid")));
        g.s(r, g.c("ID"), "1".to_string());
        g.s(r, g.c("PG"), s.gr(i, s.c(mw_col)));
        g.s(r, g.c("QG"), "0".to_string()); g.s(r, g.c("QT"), "9999".to_string()); g.s(r, g.c("QB"), "-9999".to_string());
        g.s(r, g.c("Vs"), "1".to_string()); g.s(r, g.c("MBASE"), "100".to_string());
        g.s(r, g.c("ZR"), "0".to_string()); g.s(r, g.c("ZX"), "1".to_string()); g.s(r, g.c("RT"), "0".to_string()); g.s(r, g.c("XT"), "0".to_string());
        g.s(r, g.c("GTAP"), "1".to_string()); g.s(r, g.c("STAT"), "1".to_string()); g.s(r, g.c("RMPCT"), "100".to_string());
        let hrl = s.gr(i, s.c(hrl_col));
        g.s(r, g.c("PT"), if hrl.is_empty() { "0" } else { hrl });
        let lrl = s.gr(i, s.c(lrl_col));
        g.s(r, g.c("PB"), if lrl.is_empty() { "0" } else { lrl });
        g.s(r, g.c("O1"), "1".to_string()); g.s(r, g.c("F1"), "1".to_string());
        g.s(r, g.c("O2"), "1".to_string()); g.s(r, g.c("F2"), "0".to_string());
        g.s(r, g.c("O3"), "1".to_string()); g.s(r, g.c("F3"), "0".to_string());
        g.s(r, g.c("O4"), "1".to_string()); g.s(r, g.c("F4"), "0".to_string());
        g.s(r, g.c("WMOD"), "0".to_string()); g.s(r, g.c("WPF"), "1".to_string());
    }
}

// ── Helper: write PSSE .raw file ───────────────────────────────────────────

fn write_raw(path: &Path, bus: &Table, load: &Table, gen: &Table, line: &Table, xmr: &Table, lz: &Table, no: &Table, sl: &Table, sh: &Table, skip_disc: bool) -> Result<(), String> {
    let mut w = BufWriter::with_capacity(256*1024, fs::File::create(path).map_err(|e| format!("Create {}: {e}", path.display()))?);
    let mut wl = |s: String| -> Result<(), String> { writeln!(w, "{}", s).map_err(|e| format!("W: {e}")) };
    macro_rules! g { ($t:expr, $i:expr, $c:expr) => { $t.g($i, $t.c($c)) } }

    wl("0,100".to_string())?; wl("".to_string())?; wl("".to_string())?; wl("".to_string())?;
    // Bus
    for i in 0..bus.n() {
        let bn = bus.g(i, bus.c("Bus Number")).trim().to_string();
        if bn.is_empty() { continue; }
        let p: Vec<String> = vec![bn, g!(bus,i,"Bus Name"), g!(bus,i,"Base KV"), g!(bus,i,"IDE"), "0".to_string(), "0".to_string(), g!(bus,i,"Area"), g!(bus,i,"Zone"), "1".to_string(), "0".to_string(), g!(bus,i,"Owner")];
        wl(p.join(","))?;
    }
    wl("0 / END OF BUS DATA, BEGIN LOAD DATA".to_string())?; wl("".to_string())?;
    // Load
    for i in 0..load.n() {
        let bn = load.g(i, load.c("Bus Number")).trim().to_string();
        if bn.is_empty() { continue; }
        let p: Vec<String> = vec![bn, g!(load,i,"NumID"), g!(load,i,"Status"), g!(load,i,"Area"), g!(load,i,"Zone"), g!(load,i,"PL"), "0".to_string(), "0".to_string(), "0".to_string(), "0".to_string(), "0".to_string(), g!(load,i,"Owner")];
        wl(p.join(","))?;
    }
    wl("0 / END OF LOAD DATA, BEGIN GENERATOR DATA".to_string())?; wl("".to_string())?;
    // Gen
    for i in 0..gen.n() {
        let bn = gen.g(i, gen.c("Bus Number")).trim().to_string();
        if bn.is_empty() { continue; }
        let p: Vec<String> = vec![bn, format!("'{}'", gen.g(i, gen.c("ID")).trim()), g!(gen,i,"PG"), g!(gen,i,"QG"), g!(gen,i,"QT"), g!(gen,i,"QB"), "1.25".to_string(), "0".to_string(), "100".to_string(), "0".to_string(), "1".to_string(), "0".to_string(), "0".to_string(), "1".to_string(), "1".to_string(), "100".to_string(), g!(gen,i,"PT"), g!(gen,i,"PB"), g!(gen,i,"O1"), g!(gen,i,"F1"), g!(gen,i,"O2"), g!(gen,i,"F2"), g!(gen,i,"O3"), g!(gen,i,"F3"), g!(gen,i,"O4"), g!(gen,i,"F4")];
        wl(p.join(","))?;
    }
    wl("0 / END OF GENERATOR DATA, BEGIN BRANCH DATA".to_string())?; wl("".to_string())?;
    // Branch
    for i in 0..line.n() {
        if skip_disc && line.g(i, line.c("Type")) == "Disconnector" { continue; }
        let fb = line.g(i, line.c("From Bus")).trim().to_string();
        let tb2 = line.g(i, line.c("To Bus")).trim().to_string();
        if fb.is_empty() || tb2.is_empty() { continue; }
        let p: Vec<String> = vec![fb, tb2, format!("'{}'", line.g(i, line.c("CKT")).trim()), g!(line,i,"R PU"), g!(line,i,"X PU"), g!(line,i,"B PU"), g!(line,i,"Rate A"), g!(line,i,"Rate B"), g!(line,i,"Rate C"), "0".to_string(), "0".to_string(), "0".to_string(), "0".to_string(), g!(line,i,"Status"), g!(line,i,"Length"), g!(line,i,"O1"), g!(line,i,"F1"), g!(line,i,"O2"), g!(line,i,"F2"), g!(line,i,"O3"), g!(line,i,"F3"), g!(line,i,"O4"), g!(line,i,"F4")];
        wl(p.join(","))?;
    }
    wl("0 / END OF BRANCH DATA, BEGIN TRANSFORMER DATA".to_string())?; wl("".to_string())?;
    // Transformer
    for i in 0..xmr.n() {
        let fb = xmr.g(i, xmr.c("From Bus")).trim().to_string();
        let tb2 = xmr.g(i, xmr.c("To Bus")).trim().to_string();
        if fb.is_empty() || tb2.is_empty() { continue; }
        let p: Vec<String> = vec![fb, tb2, "0".to_string(), format!("'{}'", xmr.g(i, xmr.c("CKT")).trim()), g!(xmr,i,"CW"), g!(xmr,i,"CZ"), g!(xmr,i,"CM"), g!(xmr,i,"Mag1"), g!(xmr,i,"Mag2"), g!(xmr,i,"NMETR"), g!(xmr,i,"Name"), g!(xmr,i,"STAT"), g!(xmr,i,"O1"), g!(xmr,i,"F1"), g!(xmr,i,"O2"), g!(xmr,i,"F2"), g!(xmr,i,"O3"), g!(xmr,i,"F3"), g!(xmr,i,"O4"), g!(xmr,i,"F4")];
        wl(p.join(","))?;
        wl(format!("{},{},100", g!(xmr,i,"R1-2"), g!(xmr,i,"X1-2")))?;
        wl(format!("1,0,0,{},{},{},0,0,1.1,0.9,1.1,0.9,33,0,0,0", g!(xmr,i,"RATE A"), g!(xmr,i,"RATE B"), g!(xmr,i,"RATE C")))?;
        wl("1,0".to_string())?;
    }
    wl("0 / END OF TRANSFORMER DATA, BEGIN AREA DATA".to_string())?; wl("".to_string())?;
    wl("1,0,0.0,10,'1'".to_string())?; wl("".to_string())?;
    for i in 0..lz.n() { wl(format!("{},0,0.0,10,'{}',", g!(lz,i,"Load Zone ID"), g!(lz,i,"etx-SettlementLoadZone-cim:IdentifiedObject.name")))?; wl("".to_string())?; }
    for i in 0..no.n() { wl(format!("{},0,0.0,10,'{}',", g!(no,i,"NOIE Load Zone ID"), g!(no,i,"etx-SettlementNOIELoadZone-cim:IdentifiedObject.name")))?; wl("".to_string())?; }
    wl("0 / END OF AREA DATA, BEGIN TWO-TERMINAL DC DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF TWO-TERMINAL DC DATA, BEGIN VSC DC LINE DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF VSC DC LINE DATA, BEGIN SWITCHED SHUNT DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF SWITCHED SHUNT DATA, BEGIN IMPEDANCE CORRECTION DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF IMPEDANCE CORRECTION DATA, BEGIN MULTI-TERMINAL DC DATA".to_string())?; wl("".to_string())?;
    wl("0 /END OF MULTI-TERMINAL DC DATA, BEGIN MULTI-SECTION LINE DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF MULTI-SECTION LINE DATA, BEGIN ZONE DATA".to_string())?; wl("".to_string())?;
    wl("1,'1'".to_string())?; wl("".to_string())?;
    for i in 0..sl.n() { wl(format!("{},'{}',", g!(sl,i,"Demand Zone ID"), g!(sl,i,"cim-SubLoadArea-cim:IdentifiedObject.name")))?; wl("".to_string())?; }
    wl("".to_string())?; wl("0 / END OF ZONE DATA, BEGIN INTER-AREA TRANSFER DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF INTER-AREA TRANSFER DATA, BEGIN OWNER DATA".to_string())?; wl("".to_string())?;
    wl("1,'1'".to_string())?; wl("".to_string())?;
    for i in 0..sh.n() { wl(format!("{},'{}',", g!(sh,i,"HubID"), g!(sh,i,"etx-SettlementHUB-cim:IdentifiedObject.name")))?; wl("".to_string())?; }
    wl("".to_string())?; wl("0 / END OF OWNER DATA, BEGIN FACTS DEVICE DATA".to_string())?; wl("".to_string())?;
    wl("0 / END OF FACTS DEVICE DATA".to_string())?; wl("".to_string())?;
    w.flush().map_err(|e| format!("F: {e}"))?; Ok(())
}
