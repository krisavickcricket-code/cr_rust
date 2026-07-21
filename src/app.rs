//! eframe / egui GUI for the CR extractor, model builder, SSWG mapping, and industrial load processor.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::industrial_load::{self, Progress as IndustrialProgress};
use crate::model_builder::{self, Progress as ModelProgress};
use crate::processor::{self, Progress};
use crate::sswg_mapping::{self, Progress as SswgProgress};

enum Mode { Idle, Extracting, Building, SswgMapping, IndustrialLoad }

pub struct CrApp {
    xml_path: String,
    out_folder: String,
    model_folder: String,
    planning_raw: String,
    log_lines: Vec<String>,
    mode: Mode,
    progress_rx: Option<Receiver<Progress>>,
    model_rx: Option<Receiver<ModelProgress>>,
    sswg_rx: Option<Receiver<SswgProgress>>,
    industrial_rx: Option<Receiver<IndustrialProgress>>,
}

impl Default for CrApp {
    fn default() -> Self {
        Self {
            xml_path: r"B:\ERCOT\CIM Data\CIM_Redacted.xml".into(),
            out_folder: r"B:\ERCOT\CIM Data\May2026 ML1 Files".into(),
            model_folder: r"B:\ERCOT\CIM Data\May2026 ML1 Files".into(),
            planning_raw: r"B:\ERCOT\CIM Data\May2026 ML1 Files\Output\23SSWG_2024_FAL2_U2_Final_06102024.raw".into(),
            log_lines: Vec::new(),
            mode: Mode::Idle,
            progress_rx: None,
            model_rx: None,
            sswg_rx: None,
            industrial_rx: None,
        }
    }
}

impl CrApp {
    fn start_extract(&mut self) {
        let xml = PathBuf::from(&self.xml_path);
        let out = PathBuf::from(&self.out_folder);
        if !xml.exists() { self.log("ERROR: XML file does not exist."); return; }
        if !out.is_dir() { self.log("ERROR: Output folder does not exist."); return; }
        let (tx, rx) = mpsc::channel::<Progress>();
        self.progress_rx = Some(rx);
        self.mode = Mode::Extracting;
        self.log("Started extraction…");
        thread::spawn(move || { processor::run(xml, out, move |p| { let _ = tx.send(p); }); });
    }

    fn start_model(&mut self) {
        let f = PathBuf::from(&self.model_folder);
        if !f.is_dir() { self.log("ERROR: Model input folder does not exist."); return; }
        let (tx, rx) = mpsc::channel::<ModelProgress>();
        self.model_rx = Some(rx);
        self.mode = Mode::Building;
        self.log("Started model build…");
        thread::spawn(move || {
            let tx2 = tx.clone();
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                model_builder::run(f, move |p| { let _ = tx2.send(p); });
            }));
            if result.is_err() { let _ = tx.send(ModelProgress::Error("Model builder panicked — check build_log.txt".into())); }
        });
    }

    fn start_sswg(&mut self) {
        let f = PathBuf::from(&self.model_folder);
        if !f.is_dir() { self.log("ERROR: Input folder does not exist."); return; }
        let (tx, rx) = mpsc::channel::<SswgProgress>();
        self.sswg_rx = Some(rx);
        self.mode = Mode::SswgMapping;
        self.log("Started SSWG mapping…");
        thread::spawn(move || {
            let tx2 = tx.clone();
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                sswg_mapping::run(f, move |p| { let _ = tx2.send(p); });
            }));
            if result.is_err() { let _ = tx.send(SswgProgress::Error("SSWG mapping panicked".into())); }
        });
    }

    fn start_industrial(&mut self) {
        let folder = PathBuf::from(&self.model_folder).join("Output");
        let planning = PathBuf::from(&self.planning_raw);
        if !folder.is_dir() { self.log("ERROR: Output folder does not exist."); return; }
        if !planning.exists() { self.log("ERROR: Planning .raw file does not exist."); return; }
        let (tx, rx) = mpsc::channel::<IndustrialProgress>();
        self.industrial_rx = Some(rx);
        self.mode = Mode::IndustrialLoad;
        self.log("Started industrial load processing…");
        thread::spawn(move || {
            let tx2 = tx.clone();
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                industrial_load::run(folder, planning, move |p| { let _ = tx2.send(p); });
            }));
            if result.is_err() { let _ = tx.send(IndustrialProgress::Error("Industrial load processor panicked".into())); }
        });
    }

    fn poll(&mut self) {
        if let Some(r) = self.progress_rx.take() {
            loop {
                match r.try_recv() {
                    Ok(Progress::Log(s)) => self.log(&s),
                    Ok(Progress::Done) => { self.log("✅ Extraction done."); self.mode = Mode::Idle; return; }
                    Ok(Progress::Error(e)) => { self.log(&format!("❌ {e}")); self.mode = Mode::Idle; return; }
                    Err(TryRecvError::Empty) => { self.progress_rx = Some(r); break; }
                    Err(TryRecvError::Disconnected) => {
                        self.log("❌ Extraction worker thread crashed."); self.mode = Mode::Idle; return; }
                }
            }
        }
        if let Some(r) = self.model_rx.take() {
            loop {
                match r.try_recv() {
                    Ok(ModelProgress::Log(s)) => self.log(&s),
                    Ok(ModelProgress::Done) => { self.log("✅ Model build done."); self.mode = Mode::Idle; return; }
                    Ok(ModelProgress::Error(e)) => { self.log(&format!("❌ {e}")); self.mode = Mode::Idle; return; }
                    Err(TryRecvError::Empty) => { self.model_rx = Some(r); break; }
                    Err(TryRecvError::Disconnected) => {
                        self.log("❌ Model builder worker thread crashed."); self.mode = Mode::Idle; return; }
                }
            }
        }
        if let Some(r) = self.sswg_rx.take() {
            loop {
                match r.try_recv() {
                    Ok(SswgProgress::Log(s)) => self.log(&s),
                    Ok(SswgProgress::Done) => { self.log("✅ SSWG mapping done."); self.mode = Mode::Idle; return; }
                    Ok(SswgProgress::Error(e)) => { self.log(&format!("❌ {e}")); self.mode = Mode::Idle; return; }
                    Err(TryRecvError::Empty) => { self.sswg_rx = Some(r); break; }
                    Err(TryRecvError::Disconnected) => {
                        self.log("❌ SSWG mapping worker thread crashed."); self.mode = Mode::Idle; return; }
                }
            }
        }
        if let Some(r) = self.industrial_rx.take() {
            loop {
                match r.try_recv() {
                    Ok(IndustrialProgress::Log(s)) => self.log(&s),
                    Ok(IndustrialProgress::Done) => { self.log("✅ Industrial load processing done."); self.mode = Mode::Idle; return; }
                    Ok(IndustrialProgress::Error(e)) => { self.log(&format!("❌ {e}")); self.mode = Mode::Idle; return; }
                    Err(TryRecvError::Empty) => { self.industrial_rx = Some(r); break; }
                    Err(TryRecvError::Disconnected) => {
                        self.log("❌ Industrial load worker thread crashed."); self.mode = Mode::Idle; return; }
                }
            }
        }
    }

    fn log(&mut self, msg: &str) { self.log_lines.push(msg.to_string()); if self.log_lines.len() > 5000 { self.log_lines.drain(0..1000); } }

    fn is_busy(&self) -> bool { !matches!(self.mode, Mode::Idle) }
}

impl eframe::App for CrApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.is_busy() { ctx.request_repaint_after(Duration::from_secs(1)); }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("CR — CIM XML Extractor, Model Builder, SSWG Mapping & Industrial Load");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("XML File:");
                ui.add(egui::TextEdit::singleline(&mut self.xml_path).desired_width(480.0).hint_text("Path to RDF/XML file"));
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("XML", &["xml"]).pick_file() { self.xml_path = p.display().to_string(); }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Extract Folder:");
                ui.add(egui::TextEdit::singleline(&mut self.out_folder).desired_width(460.0).hint_text("Output for CSV extraction"));
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() { self.out_folder = p.display().to_string(); }
                }
            });

            ui.separator();

            ui.horizontal(|ui| {
                if matches!(self.mode, Mode::Extracting) { ui.spinner(); ui.label("Extracting…"); }
                else if !self.is_busy() {
                    if ui.button("▶  Extract XML").clicked() { self.start_extract(); }
                }
            });

            ui.separator();
            ui.heading("Model Builder (CR2)");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Model Input:");
                ui.add(egui::TextEdit::singleline(&mut self.model_folder).desired_width(460.0).hint_text("Folder with extracted CSVs"));
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() { self.model_folder = p.display().to_string(); }
                }
            });

            ui.horizontal(|ui| {
                if matches!(self.mode, Mode::Building) { ui.spinner(); ui.label("Building model…"); }
                else if !self.is_busy() {
                    if ui.button("▶  Build Model").clicked() { self.start_model(); }
                }
            });

            ui.separator();
            ui.heading("SSWG Mapping");
            ui.separator();

            ui.horizontal(|ui| {
                if matches!(self.mode, Mode::SswgMapping) { ui.spinner(); ui.label("Mapping…"); }
                else if !self.is_busy() {
                    if ui.button("▶  SSWG Mapping").clicked() { self.start_sswg(); }
                }
            });

            ui.separator();
            ui.heading("Industrial Load");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Planning .raw:");
                ui.add(egui::TextEdit::singleline(&mut self.planning_raw).desired_width(460.0).hint_text("SSWG planning model .raw file"));
                if ui.button("Browse…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("raw", &["raw"]).pick_file() { self.planning_raw = p.display().to_string(); }
                }
            });

            ui.horizontal(|ui| {
                if matches!(self.mode, Mode::IndustrialLoad) { ui.spinner(); ui.label("Processing…"); }
                else if !self.is_busy() {
                    if ui.button("▶  Industrial Load").clicked() { self.start_industrial(); }
                }
            });

            ui.separator();
            ui.label("Log:");
            egui::ScrollArea::vertical().auto_shrink([false, true]).stick_to_bottom(true)
                .max_height(ui.available_height() - 10.0)
                .show(ui, |ui| {
                    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                        for line in &self.log_lines { ui.label(line); }
                    });
                });
        });
    }
}