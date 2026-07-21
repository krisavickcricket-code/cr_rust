//! Industrial Load processor — Rust port of PSSEIndustrialLoadProcessor (VB.NET)
//!
//! Reads a planning model .raw file, extracts industrial loads, maps them to CIM
//! bus numbers, and injects them into CIM.raw / CimNoDisconnector.raw files.

use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum Progress {
    Log(String),
    Done,
    Error(String),
}

struct Load {
    bus_number: i64,       // planning model bus
    cim_bus: i64,          // mapped CIM bus
    final_bus: i64,        // after discard mapping
    id: String,
    status: i64,
    area: i64,
    zone: i64,
    pl: f64,
    ql: f64,
    ip: f64,
    iq: f64,
    yp: f64,
    yq: f64,
    owner: i64,
    scale1: i64,
    scale2: i64,
    zone_name: String,
    area_name: String,
}

struct TerminalMapping {
    psse_bus: i64,
    cim_bus: i64,
    load_zone: String,
}

/// Split a PSSE .raw line by commas, respecting single/double quotes.
fn split_psse_line(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in line.chars() {
        match c {
            '\'' | '"' => { in_quotes = !in_quotes; }
            ',' if !in_quotes => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    parts.push(current.trim().to_string());
    // Remove trailing empty parts unless line ends with comma
    if !line.trim_end().ends_with(',') {
        while parts.last().map(|s| s.is_empty()).unwrap_or(false) {
            parts.pop();
        }
    }
    parts
}

fn parse_i(s: &str) -> i64 { s.trim().trim_matches(|c| c == '\'' || c == '"').parse::<i64>().unwrap_or(0) }
fn parse_f(s: &str) -> f64 { s.trim().trim_matches(|c| c == '\'' || c == '"').parse::<f64>().unwrap_or(0.0) }

pub fn run(input_folder: PathBuf, planning_raw_path: PathBuf, progress: impl FnMut(Progress) + Send + 'static) {
    let mut progress = progress;
    let start = Instant::now();
    let mut log = |m: String| { progress(Progress::Log(m)); };

    let cim_raw = input_folder.join("CIM.raw");
    let cim_no_disc = input_folder.join("CimNoDisconnector.raw");
    let terminal_csv = input_folder.join("Terminal Data.csv");
    let discard_csv = input_folder.join("Discard Bus Data.csv");
    let output_cim = input_folder.join("CIM_Industrial.raw");
    let output_no_disc = input_folder.join("CIMNoDisconnector_Industrial.raw");
    let output_csv = input_folder.join("Industrial Load.csv");

    // Verify files exist
    for (name, path) in [
        ("CIM.raw", &cim_raw),
        ("Planning .raw", &planning_raw_path),
        ("Terminal Data.csv", &terminal_csv),
        ("CimNoDisconnector.raw", &cim_no_disc),
    ] {
        if !path.exists() {
            progress(Progress::Error(format!("{name} not found: {}", path.display())));
            return;
        }
    }
    log(format!("Input folder: {}", input_folder.display()));
    log(format!("Planning model: {}", planning_raw_path.display()));

    // ── Step 1: Load terminal mappings ──
    log("Loading terminal mappings…".to_string());
    let mut terminal_mappings: Vec<TerminalMapping> = Vec::new();
    match fs::read_to_string(&terminal_csv) {
        Ok(data) => {
            let mut lines = data.lines();
            if let Some(header) = lines.next() {
                let headers: Vec<&str> = header.split(',').collect();
                let bus_idx = headers.iter().position(|h| h.trim() == "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber");
                let teid_idx = headers.iter().position(|h| h.trim() == "cim-ConnectivityNode-etx:ConnectivityNode.teid");
                let zone_idx = headers.iter().position(|h| h.trim() == "etx-SettlementLoadZone-cim:IdentifiedObject.name");
                if let (Some(bi), Some(ti), Some(zi)) = (bus_idx, teid_idx, zone_idx) {
                    for line in lines {
                        if line.is_empty() { continue; }
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() > [bi, ti, zi].iter().max().copied().unwrap_or(0) {
                            let psse_bus = parse_i(parts[bi]);
                            let cim_bus = parse_i(parts[ti].trim_matches(|c| c == '"'));
                            let load_zone = parts[zi].trim_matches(|c| c == '"').to_string();
                            if psse_bus > 0 && cim_bus > 0 && !load_zone.is_empty() {
                                terminal_mappings.push(TerminalMapping { psse_bus, cim_bus, load_zone });
                            }
                        }
                    }
                }
            }
        }
        Err(e) => { progress(Progress::Error(format!("Cannot read Terminal Data.csv: {e}"))); return; }
    }
    log(format!("Loaded {} terminal mappings", terminal_mappings.len()));

    // Build PSSE bus → (CIM bus, zone name) lookup
    let tm_idx: HashMap<i64, (i64, String)> = terminal_mappings.iter()
        .map(|m| (m.psse_bus, (m.cim_bus, m.load_zone.clone())))
        .collect();

    // ── Step 2: Parse planning model .raw — extract zones then loads ──
    log("Extracting loads from planning model…".to_string());
    let planning_data = match fs::read_to_string(&planning_raw_path) {
        Ok(d) => d,
        Err(e) => { progress(Progress::Error(format!("Cannot read planning .raw: {e}"))); return; }
    };

    // Parse zones (block 6 in PSSE .raw)
    let mut planning_zones: HashMap<i64, String> = HashMap::new();
    {
        let mut lines = planning_data.lines();
        // Skip 3 header lines
        for _ in 0..3 { let _ = lines.next(); }
        let mut block = 0;
        for line in lines {
            let t = line.trim();
            if t.starts_with('0') && t.contains('/') {
                block += 1;
                if block == 6 {
                    // Zone section
                    for line2 in planning_data.lines().skip(3) {
                        let t2 = line2.trim();
                        if t2.starts_with('0') { break; }
                        let parts = split_psse_line(t2);
                        if parts.len() >= 2 {
                            let zn = parse_i(&parts[0]);
                            if zn > 0 {
                                planning_zones.insert(zn, parts[1].trim_matches(|c| c == '\'' || c == '"').to_string());
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    log(format!("Found {} zones in planning model", planning_zones.len()));

    // Parse loads (block 2 in PSSE .raw)
    let mut extracted_loads: Vec<Load> = Vec::new();
    {
        let mut lines = planning_data.lines();
        for _ in 0..3 { let _ = lines.next(); }
        let mut block = 0;
        for line in lines.by_ref() {
            let t = line.trim();
            if t.starts_with('0') && t.contains('/') {
                block += 1;
                if block == 2 {
                    // Load section
                    for line2 in lines.by_ref() {
                        let t2 = line2.trim();
                        if t2.starts_with('0') { break; }
                        if t2.is_empty() { continue; }
                        let parts = split_psse_line(t2);
                        if parts.len() >= 13 {
                            let bus = parse_i(&parts[0]);
                            let id = parts[1].trim_matches(|c| c == '\'' || c == '"').to_string();
                            let status = parse_i(&parts[2]);
                            let area = parse_i(&parts[3]);
                            let zone = parse_i(&parts[4]);
                            let pl = parse_f(&parts[5]);
                            let ql = parse_f(&parts[6]);
                            let ip = parse_f(&parts[7]);
                            let iq = parse_f(&parts[8]);
                            let yp = parse_f(&parts[9]);
                            let yq = parse_f(&parts[10]);
                            let owner = parse_i(&parts[11]);
                            let scale1 = parse_i(&parts[12]);
                            let scale2 = if parts.len() >= 14 { parse_i(&parts[13]) } else { 0 };
                            let zone_name = planning_zones.get(&zone).cloned().unwrap_or_default();

                            // Skip zero MW
                            if pl == 0.0 { continue; }

                            // Check criteria: zone name CNPDOWSS/CNP_SS or ID starts with S
                            let matches = zone_name.trim() == "CNPDOWSS" || zone_name.trim() == "CNP_SS"
                                || id.trim().to_uppercase().starts_with('S');
                            if matches {
                                extracted_loads.push(Load {
                                    bus_number: bus, cim_bus: 0, final_bus: 0,
                                    id, status, area, zone, pl, ql, ip, iq, yp, yq,
                                    owner, scale1, scale2, zone_name, area_name: String::new(),
                                });
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    log(format!("Extracted {} loads matching criteria", extracted_loads.len()));

    // ── Step 3: Map bus numbers (planning → CIM) ──
    log("Mapping bus numbers…".to_string());
    let mut i = 0;
    while i < extracted_loads.len() {
        if let Some((cim_bus, zone_name)) = tm_idx.get(&extracted_loads[i].bus_number) {
            extracted_loads[i].cim_bus = *cim_bus;
            extracted_loads[i].area_name = zone_name.clone();
            i += 1;
        } else {
            extracted_loads.swap_remove(i);
        }
    }
    log(format!("Final loads to process: {}", extracted_loads.len()));

    // ── Step 4: Load discard bus mappings ──
    let mut discard_map: HashMap<i64, i64> = HashMap::new();
    if discard_csv.exists() {
        if let Ok(data) = fs::read_to_string(&discard_csv) {
            let mut lines = data.lines();
            if let Some(header) = lines.next() {
                let headers: Vec<&str> = header.split(',').collect();
                let di = headers.iter().position(|h| h.trim() == "Discarded Bus Number");
                let ni = headers.iter().position(|h| h.trim() == "New Bus Number");
                if let (Some(di), Some(ni)) = (di, ni) {
                    for line in lines {
                        if line.is_empty() { continue; }
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() > di.max(ni) {
                            let disc = parse_i(parts[di].trim_matches(|c| c == '"'));
                            let newb = parse_i(parts[ni].trim_matches(|c| c == '"'));
                            if disc > 0 && newb > 0 {
                                discard_map.insert(disc, newb);
                            }
                        }
                    }
                }
            }
        }
    }
    log(format!("Loaded {} discard bus mappings", discard_map.len()));

    // ── Step 5: Apply discard mappings ──
    for load in &mut extracted_loads {
        load.final_bus = *discard_map.get(&load.cim_bus).unwrap_or(&load.cim_bus);
    }

    // ── Step 6: Parse existing areas from CIM.raw ──
    log("Parsing existing areas from CIM.raw…".to_string());
    let cim_data = fs::read_to_string(&cim_raw).unwrap_or_default();
    let mut existing_areas: HashMap<String, i64> = HashMap::new();
    {
        let mut in_area = false;
        for line in cim_data.lines() {
            let t = line.trim();
            if t.to_uppercase().contains("BEGIN AREA DATA") { in_area = true; continue; }
            if in_area && t.starts_with('0') && t.contains('/') { break; }
            if in_area && !t.is_empty() && !t.starts_with('0') {
                let parts = split_psse_line(t);
                if parts.len() >= 5 {
                    let an = parse_i(&parts[0]);
                    if an > 0 {
                        let name = parts[4].trim().to_string();
                        if !name.is_empty() { existing_areas.insert(name, an); }
                    }
                }
            }
        }
    }
    log(format!("Found {} areas in CIM file", existing_areas.len()));

    // ── Step 7: Cross-reference areas ──
    let mut area_mapping: HashMap<String, i64> = HashMap::new();
    for load in &extracted_loads {
        if !load.area_name.is_empty() {
            let an = *existing_areas.get(&load.area_name).unwrap_or(&1);
            area_mapping.entry(load.area_name.clone()).or_insert(an);
        }
    }

    // ── Step 8: Create modified CIM files ──
    let industrial_zone = 1000000i64;

    for (input_path, output_path, label) in [
        (&cim_raw, &output_cim, "CIM_Industrial.raw"),
        (&cim_no_disc, &output_no_disc, "CIMNoDisconnector_Industrial.raw"),
    ] {
        log(format!("Creating {label}…"));
        let data = match fs::read_to_string(input_path) {
            Ok(d) => d,
            Err(e) => { log(format!("WARN: Cannot read {}: {e}", input_path.display())); continue; }
        };
        let mut w = match fs::File::create(output_path) {
            Ok(f) => BufWriter::new(f),
            Err(e) => { log(format!("ERROR: Cannot create {}: {e}", output_path.display())); continue; }
        };

        let mut in_load = false;
        let mut in_zone = false;
        for line in data.lines() {
            let t = line.trim();
            if t.to_uppercase().contains("BEGIN LOAD DATA") { in_load = true; in_zone = false; }
            else if t.to_uppercase().contains("BEGIN ZONE DATA") { in_load = false; in_zone = true; }
            else if t.starts_with('0') && t.contains('/') {
                if in_load && t.to_uppercase().contains("END OF LOAD DATA") {
                    // Inject industrial loads
                    for load in &extracted_loads {
                        let area = area_mapping.get(&load.area_name).copied().unwrap_or(1);
                        let _ = writeln!(w, "{}, '{}', {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}",
                            load.final_bus, load.id, load.status, area, industrial_zone,
                            load.pl, load.ql, load.ip, load.iq, load.yp, load.yq,
                            load.owner, load.scale1, load.scale2);
                    }
                    in_load = false;
                } else if in_zone && t.to_uppercase().contains("END OF ZONE DATA") {
                    let _ = writeln!(w, "{}, 'Industrial'", industrial_zone);
                    in_zone = false;
                }
            }
            let _ = writeln!(w, "{line}");
        }
        let _ = w.flush();
        log(format!("Created {}", output_path.display()));
    }

    // ── Step 9: Export Industrial Load.csv ──
    log("Exporting Industrial Load.csv…".to_string());
    if let Ok(mut w) = fs::File::create(&output_csv).map(BufWriter::new) {
        let _ = writeln!(w, "Original_Planning_Bus,CIM_Bus_Number,Final_Bus_Number,Load_ID,Status,Area,Zone,Zone_Name,Area_Name,MW_Load,MVAR_Load,IP,IQ,YP,YQ,Owner,Scale1,Scale2");
        for load in &extracted_loads {
            let _ = writeln!(w, "{},{},{},\"{}\",{},{},{},\"{}\",\"{}\",{},{},{},{},{},{},{},{},{}",
                load.bus_number, load.cim_bus, load.final_bus, load.id,
                load.status, load.area, load.zone, load.zone_name, load.area_name,
                load.pl, load.ql, load.ip, load.iq, load.yp, load.yq,
                load.owner, load.scale1, load.scale2);
        }
        let _ = w.flush();
        log(format!("Exported {} load records to {}", extracted_loads.len(), output_csv.display()));
    }

    log(format!("Industrial load processing complete in {:.1}s", start.elapsed().as_secs_f64()));
    progress(Progress::Done);
}