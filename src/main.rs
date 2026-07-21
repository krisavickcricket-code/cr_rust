//! Entry point for the CR XML-to-CSV extractor GUI.

mod app;
mod industrial_load;
mod model_builder;
mod processor;
mod sswg_mapping;
mod table;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title("CR — CIM XML to CSV Extractor & Model Builder"),
        ..Default::default()
    };

    eframe::run_native(
        "CR Extractor",
        options,
        Box::new(|_cc| Ok(Box::new(app::CrApp::default()))),
    )
}