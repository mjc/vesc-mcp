use std::collections::{HashMap, HashSet};
use std::env;
use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};

#[derive(Default)]
struct Report {
    samples: usize,
    self_samples: HashMap<String, usize>,
    inclusive_samples: HashMap<String, usize>,
    categories: HashMap<&'static str, usize>,
}

impl Report {
    fn add_sample(&mut self, frames: &[String]) {
        let Some(leaf) = frames.first() else {
            return;
        };

        self.samples += 1;
        *self.self_samples.entry(leaf.clone()).or_default() += 1;
        *self.categories.entry(categorize(leaf)).or_default() += 1;

        let mut seen = HashSet::new();
        for frame in frames {
            if seen.insert(frame) {
                *self.inclusive_samples.entry(frame.clone()).or_default() += 1;
            }
        }
    }
}

fn main() {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "parse_perfdata".into());
    let path = match args.next() {
        Some(arg) if arg == "-h" || arg == "--help" => {
            print_usage(&program);
            return;
        }
        Some(arg) => arg,
        None => "perf.data".into(),
    };
    if args.next().is_some() {
        print_usage(&program);
        std::process::exit(2);
    }

    let mut child = Command::new("perf")
        .args(["script", "-i", &path])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| {
            eprintln!("failed to run perf script: {error}");
            std::process::exit(1);
        });
    let stdout = child.stdout.take().expect("perf stdout was piped");
    let report = parse_perf_script(BufReader::new(stdout)).unwrap_or_else(|error| {
        eprintln!("failed to parse perf script output: {error}");
        std::process::exit(1);
    });
    let status = child.wait().unwrap_or_else(|error| {
        eprintln!("failed waiting for perf script: {error}");
        std::process::exit(1);
    });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    if report.samples == 0 {
        eprintln!("no symbolized callchains found in {path}");
        std::process::exit(1);
    }

    print_report(&report);
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [perf.data]");
    eprintln!("Reports symbolized self and inclusive CPU samples via perf script.");
}

fn parse_perf_script(reader: impl BufRead) -> io::Result<Report> {
    let mut report = Report::default();
    let mut frames = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            report.add_sample(&frames);
            frames.clear();
        } else if line.starts_with(char::is_whitespace) {
            if let Some(symbol) = parse_frame(&line) {
                frames.push(symbol);
            }
        } else if !frames.is_empty() {
            report.add_sample(&frames);
            frames.clear();
        }
    }
    report.add_sample(&frames);
    Ok(report)
}

fn parse_frame(line: &str) -> Option<String> {
    let mut fields = line.trim().splitn(2, char::is_whitespace);
    let ip = fields.next()?.trim_start_matches("0x");
    if ip.is_empty() || !ip.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let symbol_and_dso = fields.next()?.trim();
    let symbol = symbol_and_dso
        .rsplit_once(" (")
        .map_or(symbol_and_dso, |(symbol, _)| symbol);
    let symbol = symbol
        .split_once("+0x")
        .map_or(symbol, |(symbol, _)| symbol)
        .trim();
    (!symbol.is_empty()).then(|| symbol.to_string())
}

fn print_report(report: &Report) {
    println!("=== CPU samples ({} total) ===\n", report.samples);
    print_counts(
        "Self/on-CPU functions",
        &report.self_samples,
        report.samples,
        40,
    );
    println!();
    print_categories(report);
    println!();
    print_counts(
        "Inclusive functions",
        &report.inclusive_samples,
        report.samples,
        40,
    );
}

fn print_counts(title: &str, counts: &HashMap<String, usize>, total: usize, limit: usize) {
    let mut counts: Vec<_> = counts.iter().collect();
    counts.sort_unstable_by(|(left_name, left), (right_name, right)| {
        right.cmp(left).then_with(|| left_name.cmp(right_name))
    });

    println!("=== {title} ===\n");
    println!("{:>7} {:>10}  Function", "%", "samples");
    println!("{}", "-".repeat(100));
    for (name, count) in counts.into_iter().take(limit) {
        println!(
            "{:>6.2}% {:>10}  {}",
            *count as f64 / total as f64 * 100.0,
            count,
            truncate(name, 78)
        );
    }
}

fn print_categories(report: &Report) {
    let mut categories: Vec<_> = report.categories.iter().collect();
    categories.sort_unstable_by(|(left_name, left), (right_name, right)| {
        right.cmp(left).then_with(|| left_name.cmp(right_name))
    });

    println!("=== Self/on-CPU categories ===\n");
    println!("{:>7} {:>10}  Category", "%", "samples");
    println!("{}", "-".repeat(60));
    for (category, count) in categories {
        println!(
            "{:>6.2}% {:>10}  {}",
            *count as f64 / report.samples as f64 * 100.0,
            count,
            category
        );
    }
}

fn categorize(function: &str) -> &'static str {
    let lower = function.to_ascii_lowercase();
    if lower.contains("tantivy") || lower.contains("fst") {
        "tantivy/fst"
    } else if lower.contains("gix") || lower.contains("gitoxide") {
        "git/gix"
    } else if lower.contains("fastembed")
        || lower.contains("onnx")
        || lower.contains("ort::")
        || lower.contains("migraphx")
        || lower.contains("semantic")
    {
        "semantic/onnx"
    } else if lower.contains("vesc_knowledge_index") || lower.contains("vesc_mcp") {
        "vesc index"
    } else if lower.contains("alloc")
        || lower.contains("malloc")
        || lower.contains("free")
        || lower.contains("memcpy")
        || lower.contains("copy")
        || lower.contains("clone")
    {
        "allocation/copy"
    } else if lower.contains("tokio")
        || lower.contains("poll")
        || lower.contains("wake")
        || lower.contains("task")
        || lower.contains("runtime")
    {
        "tokio/runtime"
    } else if lower.contains("syscall")
        || lower.contains("read")
        || lower.contains("write")
        || lower.contains("io_uring")
        || lower.contains("fs::")
    {
        "syscall/kernel/io"
    } else {
        "other"
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let prefix: String = value.chars().take(max.saturating_sub(3)).collect();
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symbolized_perf_frame() {
        assert_eq!(
            parse_frame("        55d4d85a vesc_knowledge_index::lexical::build+0x2a (/tmp/server)")
                .as_deref(),
            Some("vesc_knowledge_index::lexical::build")
        );
    }

    #[test]
    fn reports_leaf_as_self_and_deduplicates_recursive_inclusive_frames() {
        let input = r#"server 1 [001] 1.0: cycles:
        1 tantivy::IndexWriter::commit (/tmp/server)
        2 tantivy::IndexWriter::commit (/tmp/server)
        3 vesc_knowledge_index::build (/tmp/server)

"#;
        let report = parse_perf_script(input.as_bytes()).unwrap();

        assert_eq!(report.samples, 1);
        assert_eq!(report.self_samples["tantivy::IndexWriter::commit"], 1);
        assert_eq!(report.inclusive_samples["tantivy::IndexWriter::commit"], 1);
        assert_eq!(report.inclusive_samples["vesc_knowledge_index::build"], 1);
        assert_eq!(report.categories["tantivy/fst"], 1);
    }
}
