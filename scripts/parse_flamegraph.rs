use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;

struct Entry {
    name: String,
    samples: u64,
    percent: f64,
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <flamegraph.svg> [command] [args...]", args[0]);
        eprintln!();
        eprintln!("Commands:");
        eprintln!("  top [N] [min%]     Show top N functions (default: 30, min: 1.0%)");
        eprintln!("  search <pattern>   Search for functions matching pattern");
        eprintln!("  syscalls           Show syscall breakdown");
        eprintln!("  summary            Show categorized summary");
        eprintln!("  diff <other.svg>   Compare two flamegraphs (show gained/lost CPU)");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} flamegraph.svg top 20", args[0]);
        eprintln!("  {} flamegraph.svg search tantivy", args[0]);
        eprintln!("  {} flamegraph.svg syscalls", args[0]);
        eprintln!("  {} flamegraph.svg summary", args[0]);
        eprintln!("  {} before.svg diff after.svg", args[0]);
        std::process::exit(1);
    }

    let svg_path = &args[1];
    let command = args.get(2).map(|s| s.as_str()).unwrap_or("top");

    let content = fs::read_to_string(svg_path)?;
    let entries = parse_entries(&content);

    match command {
        "top" => {
            let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
            let min_pct: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            cmd_top(&entries, n, min_pct);
        }
        "search" => {
            let pattern = args.get(3).map(|s| s.as_str()).unwrap_or("");
            cmd_search(&entries, pattern);
        }
        "syscalls" => {
            cmd_syscalls(&entries);
        }
        "summary" => {
            cmd_summary(&entries);
        }
        "diff" => {
            let other_path = match args.get(3) {
                Some(p) => p,
                None => {
                    eprintln!("Usage: {} <before.svg> diff <after.svg>", args[0]);
                    std::process::exit(1);
                }
            };
            let other_content = fs::read_to_string(other_path)?;
            let other_entries = parse_entries(&other_content);
            cmd_diff(&entries, &other_entries);
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn parse_entries(content: &str) -> Vec<Entry> {
    let mut results = Vec::new();

    for chunk in content.split("<title>") {
        if let Some(end) = chunk.find("</title>") {
            let title = &chunk[..end];
            if let Some((name, samples, percent)) = parse_title(title) {
                results.push(Entry {
                    name,
                    samples,
                    percent,
                });
            }
        }
    }

    results.sort_by(|a, b| {
        b.percent
            .partial_cmp(&a.percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

fn aggregate_entries(entries: &[Entry]) -> Vec<Entry> {
    let mut aggregate: HashMap<&str, (u64, f64)> = HashMap::new();
    for entry in entries {
        let total = aggregate.entry(&entry.name).or_default();
        total.0 += entry.samples;
        total.1 += entry.percent;
    }

    let mut entries: Vec<_> = aggregate
        .into_iter()
        .map(|(name, (samples, percent))| Entry {
            name: name.to_string(),
            samples,
            percent,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.percent
            .partial_cmp(&a.percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries
}

fn parse_title(title: &str) -> Option<(String, u64, f64)> {
    // Format: "function_name (123,456,789 samples, 12.34%)"
    let paren_start = title.rfind('(')?;
    let name = title[..paren_start].trim().to_string();
    let meta = &title[paren_start + 1..];

    let samples_end = meta.find(" samples")?;
    let samples_str = &meta[..samples_end].replace(',', "");
    let samples: u64 = samples_str.parse().ok()?;

    let pct_start = meta.rfind(", ")? + 2;
    let pct_end = meta.rfind('%')?;
    let percent: f64 = meta[pct_start..pct_end].parse().ok()?;

    if name.is_empty() || name == "all" {
        return None;
    }

    Some((name, samples, percent))
}

fn cmd_top(entries: &[Entry], n: usize, min_pct: f64) {
    let entries = aggregate_entries(entries);
    println!("Top {n} inclusive functions (>= {min_pct:.1}%):\n");
    println!("{:>7} {:>10}  {}", "%", "samples", "Function");
    println!("{}", "-".repeat(90));

    let mut shown = 0;
    let mut total = 0.0;

    for e in &entries {
        if e.percent < min_pct {
            continue;
        }
        if shown >= n {
            break;
        }

        let display_name = truncate_name(&e.name, 65);
        println!("{:>6.2}% {:>10}  {}", e.percent, e.samples, display_name);
        total += e.percent;
        shown += 1;
    }

    println!("{}", "-".repeat(90));
    println!(
        "{:>6.2}%             Total ({} functions shown)",
        total, shown
    );
}

fn cmd_search(entries: &[Entry], pattern: &str) {
    let entries = aggregate_entries(entries);
    let pattern_lower = pattern.to_lowercase();
    println!("Functions matching '{}':\n", pattern);
    println!("{:>7} {:>10}  {}", "%", "samples", "Function");
    println!("{}", "-".repeat(90));

    let mut total = 0.0;
    let mut count = 0;

    for e in &entries {
        if e.name.to_lowercase().contains(&pattern_lower) {
            let display_name = truncate_name(&e.name, 65);
            println!("{:>6.2}% {:>10}  {}", e.percent, e.samples, display_name);
            total += e.percent;
            count += 1;
        }
    }

    println!("{}", "-".repeat(90));
    println!("{:>6.2}%             Total ({} matches)", total, count);
}

fn cmd_syscalls(entries: &[Entry]) {
    let entries = aggregate_entries(entries);
    println!("Syscall breakdown:\n");
    println!("{:>7}  {}", "%", "Syscall");
    println!("{}", "-".repeat(60));

    let mut total = 0.0;

    for e in &entries {
        if e.name.starts_with("__x64_sys_") || e.name.starts_with("__x86_sys_") {
            let syscall_name = e
                .name
                .strip_prefix("__x64_sys_")
                .or_else(|| e.name.strip_prefix("__x86_sys_"))
                .unwrap_or(&e.name);
            println!("{:>6.2}%  {}", e.percent, syscall_name);
            total += e.percent;
        }
    }

    println!("{}", "-".repeat(60));
    println!("{:>6.2}%  Total syscall time", total);
}

fn cmd_summary(entries: &[Entry]) {
    let entries = aggregate_entries(entries);
    let mut categories: HashMap<&str, f64> = HashMap::new();

    for e in &entries {
        let cat = categorize(&e.name);
        *categories.entry(cat).or_insert(0.0) += e.percent;
    }

    let mut cats: Vec<_> = categories.into_iter().collect();
    cats.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    println!("Inclusive category stack presence:");
    println!("categories overlap, so the total is not constrained to 100%\n");
    println!("{:>7}  {}", "%", "Category");
    println!("{}", "-".repeat(40));

    for (cat, pct) in &cats {
        println!("{:>6.2}%  {}", pct, cat);
    }

    println!("\n{}", "=".repeat(60));
    println!("Key functions by category:\n");

    for cat in &[
        "Tantivy/FST",
        "Git/Gix",
        "Semantic/ONNX",
        "VESC Index",
        "Allocation/Copy",
        "Disk I/O",
        "Tokio Runtime",
        "Locks/Futex",
    ] {
        let funcs: Vec<_> = entries
            .iter()
            .filter(|e| categorize(&e.name) == *cat && e.percent >= 0.5)
            .take(5)
            .collect();

        if !funcs.is_empty() {
            println!("{}:", cat);
            for e in funcs {
                let short = truncate_name(&e.name, 55);
                println!("  {:>5.2}%  {}", e.percent, short);
            }
            println!();
        }
    }
}

fn cmd_diff(before: &[Entry], after: &[Entry]) {
    let before = aggregate_entries(before);
    let after = aggregate_entries(after);

    // Build maps: function name -> (samples, inclusive percent)
    let before_map: HashMap<&str, (u64, f64)> = before
        .iter()
        .map(|e| (e.name.as_str(), (e.samples, e.percent)))
        .collect();
    let after_map: HashMap<&str, (u64, f64)> = after
        .iter()
        .map(|e| (e.name.as_str(), (e.samples, e.percent)))
        .collect();

    // Collect all function names
    let mut all_names: Vec<&str> = Vec::new();
    for e in &before {
        all_names.push(&e.name);
    }
    for e in &after {
        if !before_map.contains_key(e.name.as_str()) {
            all_names.push(&e.name);
        }
    }

    // Compute deltas
    struct Delta<'a> {
        name: &'a str,
        before_pct: f64,
        after_pct: f64,
        diff_pct: f64,
        before_samples: u64,
        after_samples: u64,
    }

    let mut deltas: Vec<Delta> = Vec::new();
    for name in &all_names {
        let (bs, bp) = before_map.get(name).copied().unwrap_or((0, 0.0));
        let (a_s, ap) = after_map.get(name).copied().unwrap_or((0, 0.0));
        let diff = ap - bp;
        if diff.abs() >= 0.01 {
            deltas.push(Delta {
                name,
                before_pct: bp,
                after_pct: ap,
                diff_pct: diff,
                before_samples: bs,
                after_samples: a_s,
            });
        }
    }

    // Sort by absolute delta descending
    deltas.sort_by(|a, b| {
        b.diff_pct
            .abs()
            .partial_cmp(&a.diff_pct.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Print regressions (gained CPU)
    let regressions: Vec<_> = deltas.iter().filter(|d| d.diff_pct > 0.0).collect();
    let improvements: Vec<_> = deltas.iter().filter(|d| d.diff_pct < 0.0).collect();

    println!("Flamegraph diff: before vs after\n");

    if !regressions.is_empty() {
        println!("REGRESSIONS (gained CPU):\n");
        println!(
            "{:>8} {:>8} {:>8}  {:>10} {:>10}  {}",
            "before%", "after%", "delta%", "before_n", "after_n", "Function"
        );
        println!("{}", "-".repeat(100));
        for d in regressions.iter().take(30) {
            let display_name = truncate_name(d.name, 42);
            println!(
                "{:>7.2}% {:>7.2}% {:>+7.2}%  {:>10} {:>10}  {}",
                d.before_pct,
                d.after_pct,
                d.diff_pct,
                d.before_samples,
                d.after_samples,
                display_name
            );
        }
        println!();
    }

    if !improvements.is_empty() {
        println!("IMPROVEMENTS (lost CPU):\n");
        println!(
            "{:>8} {:>8} {:>8}  {:>10} {:>10}  {}",
            "before%", "after%", "delta%", "before_n", "after_n", "Function"
        );
        println!("{}", "-".repeat(100));
        for d in improvements.iter().take(30) {
            let display_name = truncate_name(d.name, 42);
            println!(
                "{:>7.2}% {:>7.2}% {:>+7.2}%  {:>10} {:>10}  {}",
                d.before_pct,
                d.after_pct,
                d.diff_pct,
                d.before_samples,
                d.after_samples,
                display_name
            );
        }
        println!();
    }

    if regressions.is_empty() && improvements.is_empty() {
        println!("No significant differences found (threshold: 0.01%).");
    } else {
        let total_regression: f64 = regressions.iter().map(|d| d.diff_pct).sum();
        let total_improvement: f64 = improvements.iter().map(|d| d.diff_pct).sum();
        println!(
            "Summary: {:>+.2}% regressions, {:>+.2}% improvements ({} functions changed)",
            total_regression,
            total_improvement,
            deltas.len()
        );
    }
}

fn categorize(name: &str) -> &'static str {
    let lower = name.to_lowercase();

    if lower.contains("tantivy")
        || lower.contains("tantivy_fst")
        || lower.contains("tantivy-fst")
        || lower.contains("fst::")
    {
        return "Tantivy/FST";
    }

    if lower.contains("gix")
        || lower.contains("gitoxide")
        || lower.contains("git_repository")
        || lower.contains("git_corpus")
    {
        return "Git/Gix";
    }

    if lower.contains("fastembed")
        || lower.contains("onnx")
        || lower.contains("ort::")
        || lower.contains("migraphx")
        || lower.contains("semantic")
    {
        return "Semantic/ONNX";
    }

    if lower.contains("vesc_knowledge_index")
        || lower.contains("vesc_mcp")
        || lower.contains("knowledgeindex")
    {
        return "VESC Index";
    }

    if lower.contains("alloc")
        || lower.contains("malloc")
        || lower.contains("free")
        || lower.contains("mmap")
        || lower.contains("brk")
        || lower.contains("memcpy")
        || lower.contains("copy")
        || lower.contains("clone")
    {
        return "Allocation/Copy";
    }

    if lower.contains("zfs")
        || lower.contains("zpl")
        || lower.contains("zil")
        || lower.contains("vfs")
        || lower.contains("write_all")
        || lower.contains("ext4")
        || lower.contains("xfs")
        || lower.contains("btrfs")
        || lower.contains("block_")
        || lower.contains("io_uring")
        || lower.contains("pread")
        || lower.contains("pwrite")
        || lower.contains("fs::")
        || lower.contains("file::")
    {
        return "Disk I/O";
    }

    if lower.contains("futex")
        || lower.contains("mutex")
        || lower.contains("lock")
        || lower.contains("rwlock")
        || lower.contains("semaphore")
        || lower.contains("parking_lot")
    {
        return "Locks/Futex";
    }

    if lower.contains("epoll") || lower.contains("poll") || lower.contains("mio") {
        return "Event Loop";
    }

    if lower.contains("tokio") || lower.contains("runtime") {
        return "Tokio Runtime";
    }

    if lower.contains("futures") || lower.contains("async") || lower.contains("waker") {
        return "Async/Futures";
    }

    if lower.contains("schedule") || lower.contains("switch") || lower.contains("context") {
        return "Scheduling";
    }

    if name.starts_with("__x64_sys_")
        || name.starts_with("syscall")
        || name.starts_with("do_syscall")
        || name.starts_with("entry_SYSCALL")
    {
        return "Syscall";
    }

    "Other"
}

fn truncate_name(name: &str, max_len: usize) -> String {
    if name.chars().count() <= max_len {
        name.to_string()
    } else if max_len <= 3 {
        ".".repeat(max_len)
    } else {
        let prefix: String = name.chars().take(max_len - 3).collect();
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_same_function_across_stack_branches() {
        let entries = vec![
            Entry {
                name: "tantivy::commit".into(),
                samples: 10,
                percent: 10.0,
            },
            Entry {
                name: "tantivy::commit".into(),
                samples: 15,
                percent: 15.0,
            },
        ];

        let aggregate = aggregate_entries(&entries);
        assert_eq!(aggregate.len(), 1);
        assert_eq!(aggregate[0].samples, 25);
        assert_eq!(aggregate[0].percent, 25.0);
    }

    #[test]
    fn parses_inferno_title() {
        assert_eq!(
            parse_title("tantivy::commit (1,234 samples, 12.34%)"),
            Some(("tantivy::commit".into(), 1_234, 12.34))
        );
    }
}
