mod app;
mod audio;
mod cli;
mod config;
mod detection;
mod dsp;
mod latency;
mod probe;
mod state;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--calibrate" || a == "-calibrate") {
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("info"),
        )
        .format_timestamp_millis()
        .init();
        cli::run_calibration();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([500.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Audio Latency Tool",
        options,
        Box::new(|cc| Ok(Box::new(app::LatencyApp::new(cc)))),
    )
}
