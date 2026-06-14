mod app;
mod core;
mod gh;
mod model;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_title("gh-review-insight"),
        ..Default::default()
    };

    eframe::run_native(
        "gh-review-insight",
        native_options,
        Box::new(|_cc| Ok(Box::new(app::App::new("gh".to_string())))),
    )
}
