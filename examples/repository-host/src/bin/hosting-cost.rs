//! Offline Rust reader for fixed-workload cost reports. No cloud access.
use git_zero_host::benchmark_cost::{self, BenchmarkCost};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut reports = Vec::new();
    let mut output = None;
    let mut baseline = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = Some(PathBuf::from(args.next().ok_or("missing --output path")?)),
            "--baseline" => {
                baseline = Some(PathBuf::from(
                    args.next().ok_or("missing --baseline score path")?,
                ))
            }
            "--help" | "-h" => {
                println!(
                    "hosting-cost [--output SCORE.json] [--baseline PREVIOUS-SCORE.json] 4mib-cold.json 4mib-warm.json 16mib-cold.json 16mib-warm.json"
                );
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown argument {arg}").into()),
            _ => reports.push(serde_json::from_slice::<Value>(&fs::read(arg)?)?),
        }
    }
    let score = benchmark_cost::benchmark_cost(&reports)?;
    let comparison = baseline
        .map(|path| -> Result<_, Box<dyn std::error::Error>> {
            let value: Value = serde_json::from_slice(&fs::read(path)?)?;
            let previous: BenchmarkCost =
                serde_json::from_value(value.get("score").cloned().unwrap_or(value))?;
            Ok(benchmark_cost::compare_cost(&previous, &score)?)
        })
        .transpose()?;
    let result = if let Some(comparison) = &comparison {
        json!({"score":score,"comparison":comparison})
    } else {
        serde_json::to_value(&score)?
    };
    let bytes = format!("{}\n", serde_json::to_string_pretty(&result)?);
    if let Some(path) = output {
        fs::write(path, &bytes)?;
    } else {
        io::stdout().write_all(bytes.as_bytes())?;
    }
    Ok(())
}
