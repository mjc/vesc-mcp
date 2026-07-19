use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use vesc_knowledge_index::path_evaluation::{
    PathEvaluationRun, PathEvaluationSuite, evaluate_path_run,
};

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: investigation_eval <suite.json> <run.json> <report.json> <report.md>".into(),
        );
    }
    let suite: PathEvaluationSuite = serde_json::from_slice(&fs::read(&args[0])?)?;
    let run: PathEvaluationRun = serde_json::from_slice(&fs::read(&args[1])?)?;
    let report = evaluate_path_run(&suite, &run)?;
    write(&args[2], report.canonical_json().as_bytes())?;
    write(&args[3], report.to_markdown().as_bytes())?;
    Ok(())
}

fn write(path: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}
