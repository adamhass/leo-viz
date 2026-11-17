//! Measure/export optional grouped AOIs without constructing satellite graphs.
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        eprintln!("Usage: leo-job-group CONFIG.json");
        std::process::exit(1);
    }
    match leo_capture::job_grouper::export::from_config(std::path::Path::new(&args[1])) {
        Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
        Err(e) => {
            eprintln!("Job grouping failed: {e}");
            std::process::exit(1);
        }
    }
}
