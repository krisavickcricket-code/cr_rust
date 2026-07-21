//! SSWG Mapping — Rust port of CIM SSWG Mapping/Module1.vb
//! Builds mapping tables: Final Bus Table, Final Line Table, Final Transformer Table,
//! Final Load Data, Final Contingency Data, etc.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::table::{S, Table, rc, j, parse_f};

#[derive(Debug, Clone)]
pub enum Progress {
    Log(String),
    Done,
    Error(String),
}

pub fn run(input_folder: PathBuf, progress: impl FnMut(Progress) + Send + 'static) {
    let mut progress = progress;
    let start = Instant::now();
    let out = &input_folder;

    let mut log = |m: String| {
        progress(Progress::Log(m));
    };

    macro_rules! ld {
        ($f:expr, $p:expr) => { match Table::load(&input_folder.join($f), $p) { Ok(t) => t, Err(e) => { progress(Progress::Error(e)); return; } } };
    }

    log("SSWG Mapping — loading CSVs…".to_string());

    // Load all CSVs (same as CR2 plus PA, PZ, AH, SGR, C, CE)
    let cn  = ld!("cim-ConnectivityNode.csv", "cim-ConnectivityNode");
    let mut cng = ld!("etx-ConnectivityNodeGroup.csv", "etx-ConnectivityNodeGroup");
    let eb  = ld!("etx-ElectricalBus.csv", "etx-ElectricalBus");
    let vl  = ld!("cim-VoltageLevel.csv", "cim-VoltageLevel");
    let t   = ld!("cim-Terminal.csv", "cim-Terminal");
    let hb  = ld!("etx-HUBBus.csv", "etx-HUBBus");
    let mut sh  = ld!("etx-SettlementHUB.csv", "etx-SettlementHUB");
    let col = ld!("cim-ConformLoadGroup.csv", "cim-ConformLoadGroup");
    let mut cul = ld!("cim-CustomerLoad.csv", "cim-CustomerLoad");
    let _sl  = ld!("cim-SubLoadArea.csv", "cim-SubLoadArea");
    let ss  = ld!("cim-Substation.csv", "cim-Substation");
    let lz  = ld!("etx-SettlementLoadZone.csv", "etx-SettlementLoadZone");
    let no  = ld!("etx-SettlementNOIELoadZone.csv", "etx-SettlementNOIELoadZone");
    let _dm  = ld!("cim-DAM.csv", "cim-Dam");
    let mut al  = ld!("cim-AnalogLimit.csv", "cim-AnalogLimit");
    let l   = ld!("cim-Line.csv", "cim-Line");
    let mut ac  = ld!("cim-ACLineSegment.csv", "cim-ACLineSegment");
    let r   = ld!("etx-Rating.csv", "etx-Rating");
    let o   = ld!("etx-OwnerShareRating.csv", "etx-OwnerShareRating");
    let _b   = ld!("cim-Breaker.csv", "cim-Breaker");
    let _d   = ld!("cim-Disconnector.csv", "cim-Disconnector");
    let mut tw  = ld!("cim-TransformerWinding.csv", "cim-TransformerWinding");
    let pt  = ld!("cim-PowerTransformer.csv", "cim-PowerTransformer");
    let mut al1 = ld!("cim-AnalogLimit.csv", "cim-AnalogLimit");
    let _r1  = ld!("etx-Rating.csv", "etx-Rating");
    let _sc  = ld!("cim-ShuntCompensator.csv", "cim-ShuntCompensator");
    let mut rn  = ld!("etx-ResourceNode.csv", "etx-ResourceNode");
    let _tg  = ld!("cim-ThermalGeneratingUnit.csv", "cim-ThermalGeneratingUnit");
    let _wg  = ld!("etx-WindGeneratingUnit.csv", "etx-WindGeneratingUnit");
    let _ng  = ld!("etx-NuclearGeneratingUnit.csv", "etx-NuclearGeneratingUnit");
    let _hg  = ld!("cim-HydroGeneratingUnit.csv", "cim-HydroGeneratingUnit");
    let _sm  = ld!("cim-SynchronousMachine.csv", "cim-SynchronousMachine");
    let _sg  = ld!("etx-SolarGeneratingUnit.csv", "etx-SolarGeneratingUnit");
    let _sec = ld!("cim-SeriesCompensator.csv", "cim-SeriesCompensator");
    let _dc  = ld!("etx-DCTie.csv", "etx-DCTie");

    // Extra tables for SSWG
    let pa  = ld!("etx-PlanningArea.csv", "etx-PlanningArea");
    let pz  = ld!("etx-PlanningZone.csv", "etx-PlanningZone");
    let _ah  = ld!("etx-AggregateHub.csv", "etx-AggregateHub");
    let sgr = ld!("cim-SubGeographicalRegion.csv", "cim-SubGeographicalRegion");
    let c   = ld!("cim-Contingency.csv", "cim-Contingency");
    let mut ce  = ld!("etx-ContingencyElement.csv", "etx-ContingencyElement");

    log(format!("CSVs loaded: CNG={} AC={} TW={} PT={} CUL={} CE={}", cng.n(), ac.n(), tw.n(), pt.n(), cul.n(), ce.n()));

    // Numbering hubs (same as CR2)
    let hid = sh.add_col("HubID");
    for i in 0..sh.n() { sh.s(i, hid, rc((i + 2).to_string())); }

    // Rework analog limits (same as CR2)
    rework_al(&mut al);
    rework_al(&mut al1);

    // Copies needed by VB code
    let cngc = cng.clone_t(); // CNGCTable — clean copy for joins
    let acc = ac.clone_t();   // ACCTable — clean copy for contingency joins
    let lc = l.clone_t();    // LCTable — clean copy for contingency joins

    log("CNG joins (PA, PZ, VL, HB, SH, SS, SGR, LZ)…".to_string());

    // CNG joins — full joins (VB adds ALL columns from each child)
    macro_rules! JF { ($p:expr, $c:expr, $pk:expr, $ck:expr) => { if let Err(e) = j(&mut $p, &$c, $pk, $ck) { log(format!("WARN: {e}")); } } }

    JF!(cng, pa, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PlanningArea", "etx-PlanningArea-etx:PlanningArea");
    JF!(cng, pz, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PlanningZone", "etx-PlanningZone-etx:PlanningZone");
    JF!(cng, vl, "etx-ConnectivityNodeGroup-etx:PlanningBay.VoltageLevel", "cim-VoltageLevel-cim:VoltageLevel");
    JF!(cng, hb, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.HasAHUBBus", "etx-HUBBus-etx:HUBBus");
    JF!(cng, sh, "etx-HUBBus-etx:HUBBus.SettlementHub", "etx-SettlementHUB-etx:SettlementHUB");
    JF!(cng, ss, "cim-VoltageLevel-cim:VoltageLevel.MemberOf_Substation", "cim-Substation-cim:Substation");
    JF!(cng, sgr, "cim-Substation-cim:Substation.Region", "cim-SubGeographicalRegion-cim:SubGeographicalRegion");
    JF!(cng, lz, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementLoadZone-etx:SettlementLoadZone");

    log("Resource Node joins (EB, CN)…".to_string());
    JF!(rn, eb, "etx-ResourceNode-etx:ResourceNode.ElectricalBus", "etx-ElectricalBus-etx:ElectricalBus");
    JF!(rn, cn, "etx-ElectricalBus-etx:ElectricalBus.ConnectivityNode", "cim-ConnectivityNode-cim:ConnectivityNode");

    // CNG ← RN join: VB does a complex row-expansion here (CNGcopy).
    // The VB code creates CNGcopy where each CNG row that matches multiple RN rows
    // gets duplicated. We replicate this with HashMap indexing.
    log("Building CNGcopy (RN expansion)…".to_string());
    let rn_cng_col = rn.c("cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup");
    let cng_id_col = cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup");
    let rn_idx: HashMap<S, Vec<usize>> = if rn_cng_col != usize::MAX && cng_id_col != usize::MAX {
        rn.index(rn_cng_col)
    } else {
        HashMap::new()
    };

    // Build CNGcopy: for each CNG row, if it has matching RN rows, create copies
    let mut cngcopy = Table::new();
    for h in &cng.headers { cngcopy.add_col(h.as_ref()); }
    cngcopy.add_col("FLAG");
    // Also add RN columns
    for h in &rn.headers { cngcopy.add_col(h.as_ref()); }

    for i in 0..cng.n() {
        let cng_key = cng.gr(i, cng_id_col);
        let flag_col = cngcopy.c("FLAG");
        if let Some(rn_rows) = rn_idx.get(cng_key) {
            let mut first = true;
            for &rn_row in rn_rows {
                let r = cngcopy.add_row();
                for (ci, h) in cng.headers.iter().enumerate() {
                    cngcopy.s_rc(r, cngcopy.c(h.as_ref()), cng.rows[i][ci].clone());
                }
                for (ci, h) in rn.headers.iter().enumerate() {
                    cngcopy.s_rc(r, cngcopy.c(h.as_ref()), rn.rows[rn_row][ci].clone());
                }
                cngcopy.s(r, flag_col, if first { "1" } else { "1" });
                first = false;
            }
        } else {
            let r = cngcopy.add_row();
            for (ci, h) in cng.headers.iter().enumerate() {
                cngcopy.s_rc(r, cngcopy.c(h.as_ref()), cng.rows[i][ci].clone());
            }
        }
    }

    // Also update CNG itself with RN data (VB does this too)
    JF!(cng, rn, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup", "cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup");

    log("CNGcopy-CUL and NO joins…".to_string());
    // CNGcopy ← CUL (filtered by NOIE not empty)
    JF!(cngcopy, cul, "etx-ConnectivityNodeGroup-etx:PlanningBay.VoltageLevel", "cim-CustomerLoad-cim:Equipment.MemberOf_EquipmentContainer");
    // CNG ← CUL (same filter)
    JF!(cng, cul, "etx-ConnectivityNodeGroup-etx:PlanningBay.VoltageLevel", "cim-CustomerLoad-cim:Equipment.MemberOf_EquipmentContainer");
    // CNGcopy ← NO
    JF!(cngcopy, no, "cim-CustomerLoad-etx:EnergyConsumer.SettlementNOIELoadZone", "etx-SettlementNOIELoadZone-etx:SettlementNOIELoadZone");
    // CNG ← NO
    JF!(cng, no, "cim-CustomerLoad-etx:EnergyConsumer.SettlementNOIELoadZone", "etx-SettlementNOIELoadZone-etx:SettlementNOIELoadZone");

    // ── Final Bus Table ──
    log("Building Final Bus Table…".to_string());
    let mut finalbus = Table::new();
    for c in ["BUS#", "BUS_NAME", "PLANNING_AREA_NAME", "PLANNING_AREA#", "PLANNING_ZONE_NAME", "PLANNING_ZONE#", "VOLTAGE_LEVEL", "WEATHER_ZONE", "LOAD_ZONE", "HUB_ZONE", "RESOURCE_NODE"] { finalbus.add_col(c); }
    for i in 0..cngcopy.n() {
        let r = finalbus.add_row();
        finalbus.s(r, 0, cngcopy.gr(i, cngcopy.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
        finalbus.s(r, 1, cngcopy.gr(i, cngcopy.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
        finalbus.s(r, 2, cngcopy.gr(i, cngcopy.c("etx-PlanningArea-cim:IdentifiedObject.name")));
        finalbus.s(r, 3, cngcopy.gr(i, cngcopy.c("etx-PlanningArea-etx:PlanningArea.psseid")));
        finalbus.s(r, 4, cngcopy.gr(i, cngcopy.c("etx-PlanningZone-cim:IdentifiedObject.name")));
        finalbus.s(r, 5, cngcopy.gr(i, cngcopy.c("etx-PlanningZone-etx:PlanningZone.psseid")));
        finalbus.s(r, 6, cngcopy.gr(i, cngcopy.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
        finalbus.s(r, 7, cngcopy.gr(i, cngcopy.c("cim-SubGeographicalRegion-cim:IdentifiedObject.name")));
        let noie = cngcopy.gr(i, cngcopy.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name"));
        if !noie.is_empty() {
            finalbus.s(r, 8, noie);
        } else {
            finalbus.s(r, 8, cngcopy.gr(i, cngcopy.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
        }
        finalbus.s(r, 9, cngcopy.gr(i, cngcopy.c("etx-SettlementHUB-cim:IdentifiedObject.name")));
        finalbus.s(r, 10, cngcopy.gr(i, cngcopy.c("etx-ResourceNode-cim:IdentifiedObject.name")));
    }

    cngcopy.write_csv(&out.join("Connectivity Node Group.csv")).ok();
    finalbus.write_csv(&out.join("Final Bus Table.csv")).ok();

    // ── Line Table ──
    log("Building Line Table…".to_string());
    JF!(ac, l, "cim-ACLineSegment-cim:Equipment.MemberOf_EquipmentContainer", "cim-Line-cim:Line");
    // Line ID
    let lid_col = ac.add_col("Line ID");
    for i in 0..ac.n() {
        let ln = ac.gr(i, ac.c("cim-Line-cim:IdentifiedObject.name")).to_string();
        let acn = ac.gr(i, ac.c("cim-ACLineSegment-cim:IdentifiedObject.name")).to_string();
        ac.s(i, lid_col, format!("{}{}", ln, acn));
    }
    JF!(ac, o, "cim-ACLineSegment-cim:ACLineSegment", "etx-OwnerShareRating-etx:OwnerShareRating.Equipment");
    // Rating → staticRating filter
    let ac_or_col = ac.add_col("etx-Rating-etx:Rating");
    {
        let r_idx = r.index(r.c("etx-Rating-etx:Rating.OwnerShareRating"));
        let ac_osr = ac.c("etx-OwnerShareRating-etx:OwnerShareRating");
        for i in 0..ac.n() {
            let key = ac.gr(i, ac_osr);
            if let Some(rows) = r_idx.get(key) {
                for &rr in rows {
                    let rn = r.gr(rr, r.c("etx-Rating-cim:IdentifiedObject.name"));
                    if matches!(rn, "staticRating" | "Static" | "static" | "StaticRating") {
                        ac.s(i, ac_or_col, r.gr(rr, r.c("etx-Rating-etx:Rating")));
                    }
                }
            }
        }
    }
    // Capacity limits from AL
    let cla = ac.add_col("Capacity Limit A");
    let clb = ac.add_col("Capacity Limit B");
    let clc = ac.add_col("Capacity Limit C");
    {
        let al_idx = al.index(al.c("cim-AnalogLimit-cim:AnalogLimit.LimitSet"));
        for i in 0..ac.n() {
            let key = ac.gr(i, ac_or_col);
            if let Some(rows) = al_idx.get(key) {
                for &ar in rows {
                    let an = al.gr(ar, al.c("cim-AnalogLimit-cim:IdentifiedObject.name"));
                    let val = al.gr(ar, al.c("cim-AnalogLimit-cim:AnalogLimit.value"));
                    match an {
                        "normalRating" => ac.s(i, cla, val),
                        "twoHourRating" => ac.s(i, clb, val),
                        "fifteenminuteRating" => ac.s(i, clc, val),
                        _ => {}
                    }
                }
            }
        }
    }
    // Terminal → From/To CN
    let fcn = ac.add_col("From Connectivity Node");
    let tcn = ac.add_col("To Connectivity Node");
    {
        let t_idx = t.index(t.c("cim-Terminal-cim:Terminal.ConductingEquipment"));
        let ac_id = ac.c("cim-ACLineSegment-cim:ACLineSegment");
        let t_near = t.c("cim-Terminal-etx:Terminal.near");
        let t_cn = t.c("cim-Terminal-cim:Terminal.ConnectivityNode");
        for i in 0..ac.n() {
            let key = ac.gr(i, ac_id);
            if let Some(rows) = t_idx.get(key) {
                for &tr in rows {
                    let near = t.gr(tr, t_near);
                    let cn_val = t.gr(tr, t_cn);
                    if near.eq_ignore_ascii_case("true") {
                        ac.s(i, fcn, cn_val);
                    } else {
                        ac.s(i, tcn, cn_val);
                    }
                }
            }
        }
    }
    // CN → PSSE Bus Number, CNG
    let fpbn = ac.add_col("From PSSE Bus Number");
    let fcng = ac.add_col("From Connectivity Node Group");
    let tpbn = ac.add_col("To PSSE Bus Number");
    let tcng = ac.add_col("To Connectivity Node Group");
    {
        let cn_idx = cn.index(cn.c("cim-ConnectivityNode-cim:ConnectivityNode"));
        let cn_pbn = cn.c("cim-ConnectivityNode-etx:ConnectivityNode.PSSEBusNumber");
        let cn_cng = cn.c("cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup");
        for i in 0..ac.n() {
            if let Some(rows) = cn_idx.get(ac.gr(i, fcn)) {
                if let Some(&r) = rows.first() {
                    ac.s(i, fpbn, cn.gr(r, cn_pbn));
                    ac.s(i, fcng, cn.gr(r, cn_cng));
                }
            }
            if let Some(rows) = cn_idx.get(ac.gr(i, tcn)) {
                if let Some(&r) = rows.first() {
                    ac.s(i, tpbn, cn.gr(r, cn_pbn));
                    ac.s(i, tcng, cn.gr(r, cn_cng));
                }
            }
        }
    }
    // CNG → From/To substation, bus name, load zone, weather zone, area, zone
    let cng_cols_map = [
        ("FROM_SUBSTATION_FULL_NAME", "cim-Substation-cim:IdentifiedObject.description"),
        ("FROM_BUS_NAME", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName"),
        ("START_LOAD_ZONE", ""), // special: NOIE or LZ
        ("START_WEATHER_ZONE", "cim-SubGeographicalRegion-cim:IdentifiedObject.name"),
        ("START_AREA", "etx-PlanningArea-cim:IdentifiedObject.name"),
        ("START_AREA#", "etx-PlanningArea-etx:PlanningArea.psseid"),
        ("START_ZONE", "etx-PlanningZone-cim:IdentifiedObject.name"),
        ("START_ZONE#", "etx-PlanningZone-etx:PlanningZone.psseid"),
    ];
    for (out_name, _) in &cng_cols_map { ac.add_col(out_name); }
    {
        let cng_idx = cng.index(cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup"));
        for i in 0..ac.n() {
            let key = ac.gr(i, fcng);
            if let Some(rows) = cng_idx.get(key) {
                if let Some(&r) = rows.first() {
                    ac.s(i, ac.c("FROM_SUBSTATION_FULL_NAME"), cng.gr(r, cng.c("cim-Substation-cim:IdentifiedObject.description")));
                    ac.s(i, ac.c("FROM_BUS_NAME"), cng.gr(r, cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
                    let noie = cng.gr(r, cng.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name"));
                    if !noie.is_empty() {
                        ac.s(i, ac.c("START_LOAD_ZONE"), noie);
                    } else {
                        ac.s(i, ac.c("START_LOAD_ZONE"), cng.gr(r, cng.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                    }
                    ac.s(i, ac.c("START_WEATHER_ZONE"), cng.gr(r, cng.c("cim-SubGeographicalRegion-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("START_AREA"), cng.gr(r, cng.c("etx-PlanningArea-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("START_AREA#"), cng.gr(r, cng.c("etx-PlanningArea-etx:PlanningArea.psseid")));
                    ac.s(i, ac.c("START_ZONE"), cng.gr(r, cng.c("etx-PlanningZone-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("START_ZONE#"), cng.gr(r, cng.c("etx-PlanningZone-etx:PlanningZone.psseid")));
                    if ac.gr(i, fpbn).is_empty() {
                        ac.s(i, fpbn, cng.gr(r, cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                    }
                }
            }
        }
    }
    // To side
    for (out_name, _) in &[
        ("TO_SUBSTATION_FULL_NAME", ""), ("TO_BUS_NAME", ""), ("END_LOAD_ZONE", ""),
        ("END_WEATHER_ZONE", ""), ("END_AREA", ""), ("END_AREA#", ""),
        ("END_ZONE", ""), ("END_ZONE#", ""), ("VOLTAGE_LEVEL", ""),
    ] { ac.add_col(out_name); }
    {
        let cng_idx = cng.index(cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup"));
        for i in 0..ac.n() {
            let key = ac.gr(i, tcng);
            if let Some(rows) = cng_idx.get(key) {
                if let Some(&r) = rows.first() {
                    ac.s(i, ac.c("TO_SUBSTATION_FULL_NAME"), cng.gr(r, cng.c("cim-Substation-cim:IdentifiedObject.description")));
                    ac.s(i, ac.c("TO_BUS_NAME"), cng.gr(r, cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
                    let noie = cng.gr(r, cng.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name"));
                    if !noie.is_empty() {
                        ac.s(i, ac.c("END_LOAD_ZONE"), noie);
                    } else {
                        ac.s(i, ac.c("END_LOAD_ZONE"), cng.gr(r, cng.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                    }
                    ac.s(i, ac.c("END_WEATHER_ZONE"), cng.gr(r, cng.c("cim-SubGeographicalRegion-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("END_AREA"), cng.gr(r, cng.c("etx-PlanningArea-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("END_AREA#"), cng.gr(r, cng.c("etx-PlanningArea-etx:PlanningArea.psseid")));
                    ac.s(i, ac.c("END_ZONE"), cng.gr(r, cng.c("etx-PlanningZone-cim:IdentifiedObject.name")));
                    ac.s(i, ac.c("END_ZONE#"), cng.gr(r, cng.c("etx-PlanningZone-etx:PlanningZone.psseid")));
                    ac.s(i, ac.c("VOLTAGE_LEVEL"), cng.gr(r, cng.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    if ac.gr(i, tpbn).is_empty() {
                        ac.s(i, tpbn, cng.gr(r, cng.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                    }
                }
            }
        }
    }

    l.write_csv(&out.join("Initial Line Table.csv")).ok();

    // Final Line Table
    let mut finalline = Table::new();
    for c in ["EQUIPMENT_NAME","FROM_SUBSTATION_FULL_NAME","TO_SUBSTATION_FULL_NAME","FROM_BUS_NAME","TO_BUS_NAME","FROM_BUS#","TO_BUS#","CKT_ID","VOLTAGE_LEVEL","START_LOAD_ZONE","END_LOAD_ZONE","START_WEATHER_ZONE","END_WEATHER_ZONE","R","X","B","MILEAGE","Limit A","Limit B","Limit C","START_AREA","START_AREA#","END_AREA","END_AREA#","START_ZONE","START_ZONE#","END_ZONE","END_ZONE#"] { finalline.add_col(c); }
    for i in 0..ac.n() {
        let r = finalline.add_row();
        finalline.s(r, finalline.c("EQUIPMENT_NAME"), ac.gr(i, ac.c("Line ID")));
        finalline.s(r, finalline.c("FROM_SUBSTATION_FULL_NAME"), ac.gr(i, ac.c("FROM_SUBSTATION_FULL_NAME")));
        finalline.s(r, finalline.c("TO_SUBSTATION_FULL_NAME"), ac.gr(i, ac.c("TO_SUBSTATION_FULL_NAME")));
        finalline.s(r, finalline.c("FROM_BUS_NAME"), ac.gr(i, ac.c("FROM_BUS_NAME")));
        finalline.s(r, finalline.c("TO_BUS_NAME"), ac.gr(i, ac.c("TO_BUS_NAME")));
        finalline.s(r, finalline.c("FROM_BUS#"), ac.gr(i, ac.c("From PSSE Bus Number")));
        finalline.s(r, finalline.c("TO_BUS#"), ac.gr(i, ac.c("To PSSE Bus Number")));
        finalline.s(r, finalline.c("CKT_ID"), ac.gr(i, ac.c("cim-ACLineSegment-etx:Equipment.psseid")));
        finalline.s(r, finalline.c("VOLTAGE_LEVEL"), ac.gr(i, ac.c("VOLTAGE_LEVEL")));
        finalline.s(r, finalline.c("START_LOAD_ZONE"), ac.gr(i, ac.c("START_LOAD_ZONE")));
        finalline.s(r, finalline.c("END_LOAD_ZONE"), ac.gr(i, ac.c("END_LOAD_ZONE")));
        finalline.s(r, finalline.c("START_WEATHER_ZONE"), ac.gr(i, ac.c("START_WEATHER_ZONE")));
        finalline.s(r, finalline.c("END_WEATHER_ZONE"), ac.gr(i, ac.c("END_WEATHER_ZONE")));
        finalline.s(r, finalline.c("R"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.r")));
        finalline.s(r, finalline.c("X"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.x")));
        finalline.s(r, finalline.c("B"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.bch")));
        finalline.s(r, finalline.c("MILEAGE"), ac.gr(i, ac.c("cim-ACLineSegment-cim:Conductor.length")));
        finalline.s(r, finalline.c("Limit A"), ac.gr(i, ac.c("Capacity Limit A")));
        finalline.s(r, finalline.c("Limit B"), ac.gr(i, ac.c("Capacity Limit B")));
        finalline.s(r, finalline.c("Limit C"), ac.gr(i, ac.c("Capacity Limit C")));
        finalline.s(r, finalline.c("START_AREA"), ac.gr(i, ac.c("START_AREA")));
        finalline.s(r, finalline.c("START_AREA#"), ac.gr(i, ac.c("START_AREA#")));
        finalline.s(r, finalline.c("END_AREA"), ac.gr(i, ac.c("END_AREA")));
        finalline.s(r, finalline.c("END_AREA#"), ac.gr(i, ac.c("END_AREA#")));
        finalline.s(r, finalline.c("START_ZONE"), ac.gr(i, ac.c("START_ZONE")));
        finalline.s(r, finalline.c("START_ZONE#"), ac.gr(i, ac.c("START_ZONE#")));
        finalline.s(r, finalline.c("END_ZONE"), ac.gr(i, ac.c("END_ZONE")));
        finalline.s(r, finalline.c("END_ZONE#"), ac.gr(i, ac.c("END_ZONE#")));
    }
    finalline.write_csv(&out.join("Final Line Table.csv")).ok();

    // ── Transformer Table ──
    log("Building Transformer Table…".to_string());
    JF!(tw, pt, "cim-TransformerWinding-cim:TransformerWinding.MemberOf_PowerTransformer", "cim-PowerTransformer-cim:PowerTransformer");
    JF!(tw, vl, "cim-TransformerWinding-cim:ConductingEquipment.BaseVoltage", "cim-VoltageLevel-cim:VoltageLevel.BaseVoltage");
    JF!(tw, t, "cim-TransformerWinding-cim:TransformerWinding", "cim-Terminal-cim:Terminal.ConductingEquipment");
    JF!(tw, cn, "cim-Terminal-cim:Terminal.ConnectivityNode", "cim-ConnectivityNode-cim:ConnectivityNode");
    JF!(tw, cngc, "cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup");
    JF!(tw, pa, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PlanningArea", "etx-PlanningArea-etx:PlanningArea");
    JF!(tw, pz, "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PlanningZone", "etx-PlanningZone-etx:PlanningZone");
    JF!(tw, o, "cim-Terminal-cim:Terminal.ConductingEquipment", "etx-OwnerShareRating-etx:OwnerShareRating.Equipment");
    JF!(tw, r, "etx-OwnerShareRating-etx:OwnerShareRating", "etx-Rating-etx:Rating.OwnerShareRating");
    // Capacity limits
    let tw_cla = tw.add_col("Capacity Limit A");
    let tw_clb = tw.add_col("Capacity Limit B");
    let tw_clc = tw.add_col("Capacity Limit C");
    {
        let al_idx = al.index(al.c("cim-AnalogLimit-cim:AnalogLimit.LimitSet"));
        let tw_r = tw.c("etx-Rating-etx:Rating");
        for i in 0..tw.n() {
            let key = tw.gr(i, tw_r);
            if let Some(rows) = al_idx.get(key) {
                for &ar in rows {
                    let an = al.gr(ar, al.c("cim-AnalogLimit-cim:IdentifiedObject.name"));
                    let val = al.gr(ar, al.c("cim-AnalogLimit-cim:AnalogLimit.value"));
                    match an {
                        "normalRating" => tw.s(i, tw_cla, val),
                        "twoHourRating" => tw.s(i, tw_clb, val),
                        "fifteenminuteRating" => tw.s(i, tw_clc, val),
                        _ => {}
                    }
                }
            }
        }
    }
    JF!(tw, ss, "cim-PowerTransformer-cim:Equipment.MemberOf_EquipmentContainer", "cim-Substation-cim:Substation");
    JF!(tw, sgr, "cim-Substation-cim:Substation.Region", "cim-SubGeographicalRegion-cim:SubGeographicalRegion");
    JF!(tw, lz, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementLoadZone-etx:SettlementLoadZone");
    JF!(tw, no, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementNOIELoadZone-etx:SettlementNOIELoadZone");

    tw.write_csv(&out.join("Transformer Winding Table.csv")).ok();

    // Build PowerXmerstable from PT + TW
    log("Building Power Transformer Table…".to_string());
    let mut pxmrs = Table::new();
    for c in ["EQUIPMENT_FROM_STATION_NAME","SUBSTATION_FULL_NAME","EQUIPMENT_NAME","BUS_NAME","STAR_BUS_NAME","BUS#","STAR_BUS#","CKT_ID","FROM_BUS_VOLTAGE","TO_BUS_VOLTAGE","LOAD_ZONE","WEATHER_ZONE","R","X","B","Limit A","Limit B","Limit C","PLANNING_AREA","PLANNING_AREA#","PLANNING_ZONE","PLANNING_ZONE#","TW_KEY_1","TW_KEY_2","TW_KEY_1_NAME","TW_KEY_2_NAME","Transformer Kluge"] { pxmrs.add_col(c); }

    let tw_pt_idx = tw.index(tw.c("cim-PowerTransformer-cim:PowerTransformer"));
    for i in 0..pt.n() {
        let pt_id = pt.gr(i, pt.c("cim-PowerTransformer-cim:PowerTransformer"));
        let kluge = pt.gr(i, pt.c("cim-PowerTransformer-etx:PowerTransformer.TransformerKluge"));
        if let Some(tw_rows) = tw_pt_idx.get(pt_id) {
            let r = pxmrs.add_row();
            for &tw_row in tw_rows {
                let near = tw.gr(tw_row, tw.c("cim-Terminal-etx:Terminal.near"));
                let rated_kv = tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding.ratedKV"));
                let is_star = near.eq_ignore_ascii_case("false");
                // For kluge transformers, check ratedKV == "1" for star bus
                let is_star_kluge = !kluge.is_empty() && rated_kv == "1";

                if is_star || is_star_kluge {
                    pxmrs.s(r, pxmrs.c("STAR_BUS_NAME"), tw.gr(tw_row, tw.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
                    pxmrs.s(r, pxmrs.c("STAR_BUS#"), tw.gr(tw_row, tw.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                    pxmrs.s(r, pxmrs.c("TO_BUS_VOLTAGE"), tw.gr(tw_row, tw.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("TW_KEY_2"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding")));
                    pxmrs.s(r, pxmrs.c("TW_KEY_2_NAME"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:IdentifiedObject.name")));
                } else {
                    pxmrs.s(r, pxmrs.c("EQUIPMENT_FROM_STATION_NAME"), tw.gr(tw_row, tw.c("cim-Substation-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("SUBSTATION_FULL_NAME"), tw.gr(tw_row, tw.c("cim-Substation-cim:IdentifiedObject.description")));
                    pxmrs.s(r, pxmrs.c("EQUIPMENT_NAME"), tw.gr(tw_row, tw.c("cim-PowerTransformer-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("BUS_NAME"), tw.gr(tw_row, tw.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
                    pxmrs.s(r, pxmrs.c("BUS#"), tw.gr(tw_row, tw.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
                    pxmrs.s(r, pxmrs.c("R"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding.r")));
                    pxmrs.s(r, pxmrs.c("X"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding.x")));
                    pxmrs.s(r, pxmrs.c("B"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding.b")));
                    pxmrs.s(r, pxmrs.c("CKT_ID"), tw.gr(tw_row, tw.c("cim-TransformerWinding-etx:Equipment.psseid")));
                    pxmrs.s(r, pxmrs.c("FROM_BUS_VOLTAGE"), tw.gr(tw_row, tw.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("TW_KEY_1"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:TransformerWinding")));
                    pxmrs.s(r, pxmrs.c("TW_KEY_1_NAME"), tw.gr(tw_row, tw.c("cim-TransformerWinding-cim:IdentifiedObject.name")));
                    let noie = tw.gr(tw_row, tw.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name"));
                    if !noie.is_empty() {
                        pxmrs.s(r, pxmrs.c("LOAD_ZONE"), noie);
                    } else {
                        pxmrs.s(r, pxmrs.c("LOAD_ZONE"), tw.gr(tw_row, tw.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
                    }
                    pxmrs.s(r, pxmrs.c("WEATHER_ZONE"), tw.gr(tw_row, tw.c("cim-SubGeographicalRegion-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("Limit A"), tw.gr(tw_row, tw.c("Capacity Limit A")));
                    pxmrs.s(r, pxmrs.c("Limit B"), tw.gr(tw_row, tw.c("Capacity Limit B")));
                    pxmrs.s(r, pxmrs.c("Limit C"), tw.gr(tw_row, tw.c("Capacity Limit C")));
                    pxmrs.s(r, pxmrs.c("PLANNING_AREA"), tw.gr(tw_row, tw.c("etx-PlanningArea-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("PLANNING_AREA#"), tw.gr(tw_row, tw.c("etx-PlanningArea-etx:PlanningArea.psseid")));
                    pxmrs.s(r, pxmrs.c("PLANNING_ZONE"), tw.gr(tw_row, tw.c("etx-PlanningZone-cim:IdentifiedObject.name")));
                    pxmrs.s(r, pxmrs.c("PLANNING_ZONE#"), tw.gr(tw_row, tw.c("etx-PlanningZone-etx:PlanningZone.psseid")));
                }
            }
            pxmrs.s(r, pxmrs.c("Transformer Kluge"), kluge);
        }
    }

    // Final Transformer Table (remove Kluge column)
    let mut finalxmr = Table::new();
    for h in &pxmrs.headers {
        if h.as_ref() != "Transformer Kluge" { finalxmr.add_col(h.as_ref()); }
    }
    for i in 0..pxmrs.n() {
        let r = finalxmr.add_row();
        for (ci, h) in pxmrs.headers.iter().enumerate() {
            if h.as_ref() != "Transformer Kluge" {
                finalxmr.s_rc(r, finalxmr.c(h.as_ref()), pxmrs.rows[i][ci].clone());
            }
        }
    }
    finalxmr.write_csv(&out.join("Final Transformer Table.csv")).ok();

    // Transformer Kluge (distinct kluge values with Fail flag)
    let mut kluge_tbl = Table::new();
    kluge_tbl.add_col("Transformer Kluge");
    kluge_tbl.add_col("Fail");
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..pxmrs.n() {
        let k = pxmrs.gr(i, pxmrs.c("Transformer Kluge")).to_string();
        if seen.insert(k.clone()) {
            let r = kluge_tbl.add_row();
            kluge_tbl.s(r, 0, k.as_str());
            kluge_tbl.s(r, 1, if k.is_empty() { "0" } else { "1" });
        }
    }
    // Filter to Fail=1
    kluge_tbl.rows.retain(|row| row.get(1).map(|v| v.as_ref() == "1").unwrap_or(false));
    kluge_tbl.write_csv(&out.join("Transformer Kluge.csv")).ok();

    // 3WXmers
    log("Building 3WXmers…".to_string());
    let mut threewx = Table::new();
    for c in ["EQUIPMENT_FROM_STATION_NAME","SUBSTATION_FULL_NAME","EQUIPMENT_NAME_PRIMARY","EQUIPMENT_NAME_SECONDARY","EQUIPMENT_NAME_TERTIARY","BUS_NAME_PRIMARY","BUS_NAME_SECONDARY","BUS_NAME_TERTIARY","STAR_BUS_NAME","BUS#_PRIMARY","BUS#_SECONDARY","BUS#_TERTIARY","STAR_BUS#","PW_START_BUS#","CKT_ID","BUS_VOLTAGE_PRIMARY","BUS_VOLTAGE_SECONDARY","BUS_VOLTAGE_TERTIARY"] { threewx.add_col(c); }
    // Sort pxmrs by FROM_BUS_VOLTAGE descending (VB sorts by VoltageDec)
    let fbv_col = pxmrs.c("FROM_BUS_VOLTAGE");
    pxmrs.rows.sort_by(|a, b| {
        let va = parse_f(a.get(fbv_col).map(|v| v.as_ref()).unwrap_or(""));
        let vb = parse_f(b.get(fbv_col).map(|v| v.as_ref()).unwrap_or(""));
        vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal)
    });
    for i in 0..kluge_tbl.n() {
        let kluge_val = kluge_tbl.gr(i, 0).to_string();
        let r = threewx.add_row();
        let mut flag = 1;
        for j in 0..pxmrs.n() {
            if pxmrs.gr(j, pxmrs.c("Transformer Kluge")) == kluge_val.as_str() {
                threewx.s(r, threewx.c("EQUIPMENT_FROM_STATION_NAME"), pxmrs.gr(j, pxmrs.c("EQUIPMENT_FROM_STATION_NAME")));
                threewx.s(r, threewx.c("SUBSTATION_FULL_NAME"), pxmrs.gr(j, pxmrs.c("SUBSTATION_FULL_NAME")));
                threewx.s(r, threewx.c("STAR_BUS_NAME"), pxmrs.gr(j, pxmrs.c("STAR_BUS_NAME")));
                threewx.s(r, threewx.c("STAR_BUS#"), pxmrs.gr(j, pxmrs.c("STAR_BUS#")));
                threewx.s(r, threewx.c("CKT_ID"), pxmrs.gr(j, pxmrs.c("CKT_ID")));
                match flag {
                    1 => {
                        threewx.s(r, threewx.c("EQUIPMENT_NAME_PRIMARY"), pxmrs.gr(j, pxmrs.c("EQUIPMENT_NAME")));
                        threewx.s(r, threewx.c("BUS_NAME_PRIMARY"), pxmrs.gr(j, pxmrs.c("BUS_NAME")));
                        threewx.s(r, threewx.c("BUS#_PRIMARY"), pxmrs.gr(j, pxmrs.c("BUS#")));
                        threewx.s(r, threewx.c("BUS_VOLTAGE_PRIMARY"), pxmrs.gr(j, pxmrs.c("FROM_BUS_VOLTAGE")));
                        flag = 2;
                    }
                    2 => {
                        threewx.s(r, threewx.c("EQUIPMENT_NAME_SECONDARY"), pxmrs.gr(j, pxmrs.c("EQUIPMENT_NAME")));
                        threewx.s(r, threewx.c("BUS_NAME_SECONDARY"), pxmrs.gr(j, pxmrs.c("BUS_NAME")));
                        threewx.s(r, threewx.c("BUS#_SECONDARY"), pxmrs.gr(j, pxmrs.c("BUS#")));
                        threewx.s(r, threewx.c("BUS_VOLTAGE_SECONDARY"), pxmrs.gr(j, pxmrs.c("FROM_BUS_VOLTAGE")));
                        flag = 3;
                    }
                    _ => {
                        threewx.s(r, threewx.c("EQUIPMENT_NAME_TERTIARY"), pxmrs.gr(j, pxmrs.c("EQUIPMENT_NAME")));
                        threewx.s(r, threewx.c("BUS_NAME_TERTIARY"), pxmrs.gr(j, pxmrs.c("BUS_NAME")));
                        threewx.s(r, threewx.c("BUS#_TERTIARY"), pxmrs.gr(j, pxmrs.c("BUS#")));
                        threewx.s(r, threewx.c("BUS_VOLTAGE_TERTIARY"), pxmrs.gr(j, pxmrs.c("FROM_BUS_VOLTAGE")));
                    }
                }
            }
        }
    }
    threewx.write_csv(&out.join("3WXmers.csv")).ok();

    // ── Load Data ──
    log("Building Load Data…".to_string());
    JF!(cul, t, "cim-CustomerLoad-cim:CustomerLoad", "cim-Terminal-cim:Terminal.ConductingEquipment");
    JF!(cul, cn, "cim-Terminal-cim:Terminal.ConnectivityNode", "cim-ConnectivityNode-cim:ConnectivityNode");
    JF!(cul, cngc, "cim-ConnectivityNode-etx:ConnectivityNode.ConnectivityNodeGroup", "etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup");
    JF!(cul, pa, "cim-CustomerLoad-etx:EnergyConsumer.PlanningArea", "etx-PlanningArea-etx:PlanningArea");
    JF!(cul, pz, "cim-CustomerLoad-etx:EnergyConsumer.PlanningZone", "etx-PlanningZone-etx:PlanningZone");
    JF!(cul, vl, "cim-CustomerLoad-cim:Equipment.MemberOf_EquipmentContainer", "cim-VoltageLevel-cim:VoltageLevel");
    JF!(cul, ss, "cim-VoltageLevel-cim:VoltageLevel.MemberOf_Substation", "cim-Substation-cim:Substation");
    JF!(cul, sgr, "cim-Substation-cim:Substation.Region", "cim-SubGeographicalRegion-cim:SubGeographicalRegion");
    JF!(cul, lz, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementLoadZone-etx:SettlementLoadZone");
    JF!(cul, no, "cim-Substation-etx:Substation.SettlementLoadZone", "etx-SettlementNOIELoadZone-etx:SettlementNOIELoadZone");
    JF!(cul, col, "cim-CustomerLoad-cim:ConformLoad.LoadGroup", "cim-ConformLoadGroup-cim:ConformLoadGroup");

    cul.write_csv(&out.join("Customer Load Raw.csv")).ok();

    let mut finalload = Table::new();
    for c in ["BUS#","BUS_NAME","LOAD_NAME","PLANNING_AREA_NAME","PLANNING_AREA#","PLANNING_ZONE_NAME","PLANNING_ZONE#","VOLTAGE_LEVEL","WEATHER_ZONE","LOAD_ZONE","PSSE_ID","P_FIXED","P_NOM","MAX_MW"] { finalload.add_col(c); }
    for i in 0..cul.n() {
        let r = finalload.add_row();
        finalload.s(r, 0, cul.gr(i, cul.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusNumber")));
        finalload.s(r, 1, cul.gr(i, cul.c("etx-ConnectivityNodeGroup-etx:ConnectivityNodeGroup.PSSEBusName")));
        finalload.s(r, 2, cul.gr(i, cul.c("cim-ConformLoadGroup-cim:IdentifiedObject.name")));
        finalload.s(r, 3, cul.gr(i, cul.c("etx-PlanningArea-cim:IdentifiedObject.name")));
        finalload.s(r, 4, cul.gr(i, cul.c("etx-PlanningArea-etx:PlanningArea.psseid")));
        finalload.s(r, 5, cul.gr(i, cul.c("etx-PlanningZone-cim:IdentifiedObject.name")));
        finalload.s(r, 6, cul.gr(i, cul.c("etx-PlanningZone-etx:PlanningZone.psseid")));
        finalload.s(r, 7, cul.gr(i, cul.c("cim-VoltageLevel-cim:IdentifiedObject.name")));
        finalload.s(r, 8, cul.gr(i, cul.c("cim-SubGeographicalRegion-cim:IdentifiedObject.name")));
        let noie = cul.gr(i, cul.c("etx-SettlementNOIELoadZone-cim:IdentifiedObject.name"));
        if !noie.is_empty() {
            finalload.s(r, 9, noie);
        } else {
            finalload.s(r, 9, cul.gr(i, cul.c("etx-SettlementLoadZone-cim:IdentifiedObject.name")));
        }
        finalload.s(r, 10, cul.gr(i, cul.c("cim-CustomerLoad-etx:Equipment.psseid")));
        finalload.s(r, 11, cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.pfixed")));
        finalload.s(r, 12, cul.gr(i, cul.c("cim-CustomerLoad-cim:EnergyConsumer.pnom")));
        finalload.s(r, 13, cul.gr(i, cul.c("cim-CustomerLoad-etx:EnergyConsumer.maxMW")));
    }
    finalload.write_csv(&out.join("Final Load Data.csv")).ok();

    // ── Contingency Data ──
    log("Building Contingency Data…".to_string());
    JF!(ce, c, "etx-ContingencyElement-etx:ContingencyElement.MemberOf_Contingency", "cim-Contingency-cim:Contingency");
    JF!(ce, acc, "etx-ContingencyElement-etx:ContingencyElement.Equipment", "cim-ACLineSegment-cim:ACLineSegment");
    JF!(ce, lc, "cim-ACLineSegment-cim:Equipment.MemberOf_EquipmentContainer", "cim-Line-cim:Line");
    // Line ID
    let ce_lid = ce.add_col("Line ID");
    for i in 0..ce.n() {
        let ln = ce.gr(i, ce.c("cim-Line-cim:IdentifiedObject.name")).to_string();
        let acn = ce.gr(i, ce.c("cim-ACLineSegment-cim:IdentifiedObject.name")).to_string();
        ce.s(i, ce_lid, format!("{}{}", ln, acn));
    }
    // Join with final line table
    for c in ["EQUIPMENT_NAME","FROM_BUS#","TO_BUS#","CKT_ID","IS_LINE","IS_TRANSFORMER","LINE_COUNT"] { ce.add_col(c); }
    {
        let fl_idx = finalline.index(finalline.c("EQUIPMENT_NAME"));
        for i in 0..ce.n() {
            let key = ce.gr(i, ce_lid);
            if let Some(rows) = fl_idx.get(key) {
                if let Some(&r) = rows.first() {
                    ce.s(i, ce.c("EQUIPMENT_NAME"), finalline.gr(r, finalline.c("EQUIPMENT_NAME")));
                    ce.s(i, ce.c("FROM_BUS#"), finalline.gr(r, finalline.c("FROM_BUS#")));
                    ce.s(i, ce.c("TO_BUS#"), finalline.gr(r, finalline.c("TO_BUS#")));
                    ce.s(i, ce.c("CKT_ID"), finalline.gr(r, finalline.c("CKT_ID")));
                    ce.s(i, ce.c("IS_LINE"), "TRUE");
                    ce.s(i, ce.c("IS_TRANSFORMER"), "FALSE");
                }
            }
        }
    }
    // Join with PowerXmerstable on TW_KEY_1
    {
        let px_idx = pxmrs.index(pxmrs.c("TW_KEY_1"));
        let ce_eq = ce.c("etx-ContingencyElement-etx:ContingencyElement.Equipment");
        for i in 0..ce.n() {
            let key = ce.gr(i, ce_eq);
            if let Some(rows) = px_idx.get(key) {
                if let Some(&r) = rows.first() {
                    let name = format!("{}-{}-{}", pxmrs.gr(r, pxmrs.c("EQUIPMENT_FROM_STATION_NAME")), pxmrs.gr(r, pxmrs.c("EQUIPMENT_NAME")), pxmrs.gr(r, pxmrs.c("FROM_BUS_VOLTAGE")));
                    ce.s(i, ce.c("EQUIPMENT_NAME"), name.as_str());
                    ce.s(i, ce.c("FROM_BUS#"), pxmrs.gr(r, pxmrs.c("BUS#")));
                    ce.s(i, ce.c("TO_BUS#"), pxmrs.gr(r, pxmrs.c("STAR_BUS#")));
                    ce.s(i, ce.c("CKT_ID"), pxmrs.gr(r, pxmrs.c("CKT_ID")));
                    ce.s(i, ce.c("IS_LINE"), "FALSE");
                    ce.s(i, ce.c("IS_TRANSFORMER"), "TRUE");
                }
            }
        }
    }
    ce.write_csv(&out.join("Initial Contingency Data.csv")).ok();

    // Final Contingency Data
    let mut finalcont = Table::new();
    for c in ["CONTINGENCY_NAME","CONTINGENCY_DESCRIPTION","EQUIPMENT_NAME","FROM_BUS#","TO_BUS#","CKT_ID","LINE_COUNT","IS_LINE","IS_TRANSFORMER"] { finalcont.add_col(c); }
    for i in 0..ce.n() {
        let eqname = ce.gr(i, ce.c("EQUIPMENT_NAME"));
        if eqname.is_empty() { continue; }
        let r = finalcont.add_row();
        finalcont.s(r, 0, ce.gr(i, ce.c("cim-Contingency-cim:IdentifiedObject.name")));
        finalcont.s(r, 1, ce.gr(i, ce.c("cim-Contingency-cim:IdentifiedObject.description")));
        finalcont.s(r, 2, eqname);
        finalcont.s(r, 3, ce.gr(i, ce.c("FROM_BUS#")));
        finalcont.s(r, 4, ce.gr(i, ce.c("TO_BUS#")));
        finalcont.s(r, 5, ce.gr(i, ce.c("CKT_ID")));
        finalcont.s(r, 6, ce.gr(i, ce.c("LINE_COUNT")));
        finalcont.s(r, 7, ce.gr(i, ce.c("IS_LINE")));
        finalcont.s(r, 8, ce.gr(i, ce.c("IS_TRANSFORMER")));
    }
    finalcont.write_csv(&out.join("Final Contingency Data.csv")).ok();

    log(format!("SSWG Mapping complete in {:.1}s", start.elapsed().as_secs_f64()));
    progress(Progress::Done);
}

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