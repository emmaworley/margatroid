#![deny(warnings)]

mod app;
mod views;

use margatroid::session;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 {
        // Direct mode: margatroid-tui <name> [image] [--skip-permissions] [--resume-interrupted]
        let name = &args[1];
        let image = args.get(2).map(|s| s.as_str()).unwrap_or("ubuntu");
        let inject_resume = args.iter().any(|a| a == "--resume-interrupted");
        let skip_permissions = args.iter().any(|a| a == "--skip-permissions");

        if let Err(e) = session::launch(name, image, inject_resume, skip_permissions) {
            eprintln!("Failed to launch session: {e}");
            std::process::exit(1);
        }
    } else {
        // Interactive mode
        if let Err(e) = app::run() {
            eprintln!("TUI error: {e}");
            std::process::exit(1);
        }
    }
}
