use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use ctx_code::graph::graph_query;
use ctx_code::indexer::index_repository;
use ctx_code::pack::pack_query;
use ctx_code::search::search_index;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
struct Arguments {
    #[arg(long, default_value_t = 1_000)]
    files: usize,
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    #[arg(long, default_value_t = 10)]
    warmups: usize,
    #[arg(long)]
    output: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    anyhow::ensure!(arguments.files >= 10, "--files doit être >= 10");
    anyhow::ensure!(arguments.iterations > 0, "--iterations doit être > 0");
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("repo");
    fs::create_dir(&root)?;
    let (symbols, target) = generate_repository(&root, arguments.files)?;

    let started = Instant::now();
    let cold = index_repository(&root)?;
    let cold_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let cold_peak_rss_bytes = peak_rss_bytes();
    let started = Instant::now();
    let unchanged = index_repository(&root)?;
    let unchanged_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let unchanged_peak_rss_bytes = peak_rss_bytes();

    let mut metrics = serde_json::Map::new();
    metrics.insert(
        "index_cold".to_owned(),
        json!({
            "elapsed_ms": round(cold_ms),
            "peak_process_rss_bytes": cold_peak_rss_bytes,
            "files_per_second": round(cold.files as f64 / (cold_ms / 1_000.0)),
        }),
    );
    metrics.insert(
        "index_unchanged".to_owned(),
        json!({
            "elapsed_ms": round(unchanged_ms),
            "changed_files": unchanged.changed,
            "peak_process_rss_bytes": unchanged_peak_rss_bytes,
        }),
    );
    metrics.insert(
        "search_symbol".to_owned(),
        measure(arguments.iterations, arguments.warmups, |index| {
            search_index(
                &symbols[index % symbols.len()],
                "auto",
                None,
                20,
                1_500,
                &root,
            )
            .map(|_| ())
        })?,
    );
    metrics.insert(
        "search_text".to_owned(),
        measure(arguments.iterations, arguments.warmups, |_| {
            search_index("payment retry ledger", "auto", None, 20, 1_500, &root).map(|_| ())
        })?,
    );
    for mode in ["literal", "regex"] {
        let pattern = if mode == "literal" {
            "payment"
        } else {
            "payment.*ledger"
        };
        for indexed in [false, true] {
            let label = format!("{mode}_{}", if indexed { "trigram" } else { "scan" });
            metrics.insert(
                label,
                measure(arguments.iterations, arguments.warmups, |_| {
                    let search = if indexed {
                        ctx_code::text_index::exact_search
                    } else {
                        ctx_code::text_index::scan_search
                    };
                    search(pattern, mode, false, None, usize::MAX, usize::MAX, &root).map(|_| ())
                })?,
            );
        }
    }

    metrics.insert(
        "graph_def".to_owned(),
        measure(arguments.iterations, arguments.warmups, |_| {
            graph_query("def", &target, 2, 1_500, &root).map(|_| ())
        })?,
    );
    metrics.insert(
        "graph_callers".to_owned(),
        measure(arguments.iterations, arguments.warmups, |_| {
            graph_query("callers", &target, 2, 1_500, &root).map(|_| ())
        })?,
    );
    metrics.insert(
        "pack".to_owned(),
        measure(arguments.iterations, arguments.warmups, |_| {
            pack_query("retry payment ledger", 2_000, "explore", &root).map(|_| ())
        })?,
    );

    let source_bytes = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && !entry
                    .path()
                    .components()
                    .any(|part| part.as_os_str() == ".ctx")
        })
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum::<u64>();
    let result = json!({
        "schema": 2,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "methodology": {
            "fixture": "generated mixed Python/TypeScript/Go/Rust/PHP repository",
            "generated_code_files": arguments.files,
            "indexed_files": cold.files,
            "source_bytes": source_bytes,
            "iterations": arguments.iterations,
            "warmups": arguments.warmups,
            "filesystem": "temporary directory",
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "environment": {
            "platform": format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            "cpu": cpu_model(),
            "rustc": rustc_version(),
            "sqlite": rusqlite::version(),
            "logical_cpus": std::thread::available_parallelism().map(usize::from).unwrap_or(1),
            "memory_bytes": total_memory_bytes(),
        },
        "index": {
            "symbols": cold.symbols,
            "edges": cold.edges,
            "database_bytes": cold.database.metadata()?.len(),
            "trigram_bytes": directory_bytes(&root.join(".ctx/text")),
        },
        "metrics": metrics,
        "notes": [
            "All retrieval measurements are warm-process timings.",
            "Exact scan and trigram variants use identical patterns, admission, matcher and unlimited output budgets.",
            "RSS is the process lifetime high-water mark on Linux, not an isolated per-phase allocation measurement; null elsewhere.",
            "The benchmark calls the same Rust library used by the persistent MCP server.",
            "Synthetic results do not predict performance on every real repository.",
        ],
    });
    let rendered = format!("{}\n", serde_json::to_string_pretty(&result)?);
    if let Some(output) = arguments.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &rendered)?;
    }
    print!("{rendered}");
    Ok(())
}

fn measure(
    iterations: usize,
    warmups: usize,
    mut operation: impl FnMut(usize) -> Result<()>,
) -> Result<Value> {
    for index in 0..warmups {
        operation(index)?;
    }
    let mut values = Vec::with_capacity(iterations);
    for index in 0..iterations {
        let started = Instant::now();
        operation(index)?;
        values.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    values.sort_by(f64::total_cmp);
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    Ok(json!({
        "min_ms": round(values[0]),
        "p50_ms": round(percentile(&values, 0.50)),
        "p95_ms": round(percentile(&values, 0.95)),
        "p99_ms": round(percentile(&values, 0.99)),
        "max_ms": round(*values.last().unwrap()),
        "mean_ms": round(mean),
    }))
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    let position = (values.len() - 1) as f64 * fraction;
    let lower = position.floor() as usize;
    let upper = (lower + 1).min(values.len() - 1);
    values[lower] * (1.0 - position.fract()) + values[upper] * position.fract()
}

fn round(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn cpu_model() -> String {
    if let Ok(contents) = fs::read_to_string("/proc/cpuinfo")
        && let Some(model) = contents.lines().find_map(|line| {
            line.strip_prefix("model name")
                .and_then(|value| value.split_once(':'))
                .map(|(_, value)| value.trim().to_owned())
        })
    {
        return model;
    }
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn total_memory_bytes() -> Option<u64> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    let kilobytes = contents.lines().find_map(|line| {
        line.strip_prefix("MemTotal:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kilobytes.checked_mul(1_024)
}

fn generate_repository(root: &Path, files: usize) -> Result<(Vec<String>, String)> {
    let mut python_symbols = Vec::new();
    for index in 0..files {
        let language = index % 5;
        let (extension, source) = match language {
            0 => {
                python_symbols.push(format!("service_{index:05}"));
                ("py", python_source(index))
            }
            1 => ("ts", typescript_source(index)),
            2 => ("go", go_source(index)),
            3 => ("rs", rust_source(index)),
            _ => ("php", php_source(index)),
        };
        let directory = root.join(format!("package_{:02}", index % 20));
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join(format!("module_{index:05}.{extension}")),
            source,
        )?;
    }
    let target = python_symbols[python_symbols.len() / 2].clone();
    fs::write(
        root.join("callsite.py"),
        format!("def process_order(order_id: str) -> str:\n    return {target}(order_id)\n"),
    )?;
    fs::write(
        root.join("README.md"),
        "# Synthetic benchmark repository\n\nPayment retry ledger services.\n",
    )?;
    Ok((python_symbols, target))
}

fn payload(index: usize) -> String {
    format!(
        "Synthetic module {index}: {}",
        "payment retry ledger customer order transaction ".repeat(8)
    )
}

fn python_source(index: usize) -> String {
    format!(
        "\"\"\"{}\"\"\"\n\ndef normalize_{index:05}(value: str) -> str:\n    return value.strip().lower()\n\ndef service_{index:05}(order_id: str) -> str:\n    \"\"\"Retry a payment and record the ledger transaction.\"\"\"\n    return normalize_{index:05}(order_id)\n",
        payload(index)
    )
}

fn typescript_source(index: usize) -> String {
    format!(
        "// {}\nexport function normalize_{index:05}(value: string): string {{ return value.trim().toLowerCase(); }}\nexport function service_{index:05}(orderId: string): string {{ return normalize_{index:05}(orderId); }}\n",
        payload(index)
    )
}

fn go_source(index: usize) -> String {
    format!(
        "package pkg{:02}\n\n// {}\nfunc normalize_{index:05}(value string) string {{ return value }}\nfunc service_{index:05}(orderID string) string {{ return normalize_{index:05}(orderID) }}\n",
        index % 20,
        payload(index)
    )
}

fn rust_source(index: usize) -> String {
    format!(
        "// {}\npub fn normalize_{index:05}(value: &str) -> String {{ value.trim().to_lowercase() }}\npub fn service_{index:05}(order_id: &str) -> String {{ normalize_{index:05}(order_id) }}\n",
        payload(index)
    )
}

fn php_source(index: usize) -> String {
    format!(
        "<?php\n// {}\nfunction normalize_{index:05}(string $value): string {{ return strtolower(trim($value)); }}\nfunction service_{index:05}(string $orderId): string {{ return normalize_{index:05}($orderId); }}\n",
        payload(index)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_benchmark_exercises_every_operation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        let (symbols, target) = generate_repository(&root, 10).unwrap();
        let indexed = index_repository(&root).unwrap();
        assert_eq!(indexed.files, 12);
        assert!(!symbols.is_empty());

        search_index(&symbols[0], "auto", None, 20, 1_500, &root).unwrap();
        graph_query("def", &target, 2, 1_500, &root).unwrap();
        graph_query("callers", &target, 2, 1_500, &root).unwrap();
        pack_query("retry payment ledger", 2_000, "explore", &root).unwrap();

        let timings = measure(2, 1, |_| Ok(())).unwrap();
        assert!(timings["p50_ms"].is_number());
    }
}

fn directory_bytes(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })
}
