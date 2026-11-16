//! Solve precomputed acquisition graphs without regenerating orbital geometry.
use leo_capture::scheduling::solver::{diagnose, solve, solve_baseline, Config};
fn main() {
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<_> = std::env::args_os().collect();
        let diagnostic = args.len() == 5 && args[1] == "--diagnose";
        let baseline = args.len() == 3 && args[1] == "--baseline";
        if args.len() != 2 && !diagnostic && !baseline {
            return Err("Usage: leo-schedule CONFIG.json | leo-schedule --baseline CONFIG.json | leo-schedule --diagnose CONFIG.json SCHEDULE.json OUTPUT.json".into());
        }
        let path = std::fs::canonicalize(&args[if diagnostic || baseline { 2 } else { 1 }])?;
        let mut config: Config = serde_json::from_reader(std::fs::File::open(&path)?)?;
        let base = path.parent().unwrap();
        config.graph_directory = base.join(config.graph_directory);
        config.output = base.join(config.output);
        if diagnostic {
            use std::io::Write;
            let output = std::path::Path::new(&args[4]);
            if output.exists() {
                return Err("Diagnostic output already exists".into());
            }
            let result = diagnose(&config, std::path::Path::new(&args[3]))?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            file.write_all(&serde_json::to_vec_pretty(&result)?)?;
            file.write_all(b"\n")?;
            println!("{}", serde_json::to_string_pretty(&result["totals"])?);
            println!("Diagnostic report: {}", output.display());
            return Ok(());
        }
        let result = if baseline {
            solve_baseline(&config)?
        } else {
            solve(&config)?
        };
        println!("{}", serde_json::to_string_pretty(&result["verification"])?);
        println!("Report: {}", config.output.join("report.json").display());
        Ok(())
    };
    if let Err(error) = run() {
        eprintln!("Scheduling failed: {error}");
        std::process::exit(1);
    }
}
