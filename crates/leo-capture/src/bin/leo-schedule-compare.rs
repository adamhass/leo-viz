//! Run comparable optimizers across reward models and upstream workload variants.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        return Err("Usage: leo-schedule-compare CONFIG.json".into());
    }
    let path = std::fs::canonicalize(&args[1])?;
    let mut config: leo_capture::scheduling::solver::experiment::Experiment =
        serde_json::from_reader(std::fs::File::open(&path)?)?;
    let base = path.parent().ok_or("Missing config directory")?;
    config.output = base.join(config.output);
    for workload in &mut config.workloads {
        workload.graph_directory = base.join(&workload.graph_directory);
    }
    let result = leo_capture::scheduling::solver::experiment::run(&config)?;
    println!("{}", serde_json::to_string_pretty(&result["results"])?);
    println!(
        "Comparison: {}",
        config.output.join("comparison.json").display()
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Comparison failed: {e}");
        std::process::exit(1);
    }
}
