//! Optional geographic refinement of an ongoing workload's deadline phases.
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        eprintln!("Usage: leo-job-align CONFIG.json");
        std::process::exit(1);
    }
    match leo_capture::cadence_alignment::export::from_config(std::path::Path::new(&args[1])) {
        Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
        Err(e) => {
            eprintln!("Cadence alignment failed: {e}");
            std::process::exit(1);
        }
    }
}
