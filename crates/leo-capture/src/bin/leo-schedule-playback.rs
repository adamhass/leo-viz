fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        eprintln!("Usage: leo-schedule-playback SCHEDULE.json PLAYBACK.json.gz");
        std::process::exit(1);
    }
    if let Err(e) = leo_capture::scheduling::playback::export(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    ) {
        eprintln!("Replay export failed: {e}");
        std::process::exit(1);
    }
    println!(
        "Saved portable schedule replay to {}",
        std::path::Path::new(&args[2]).display()
    );
}
