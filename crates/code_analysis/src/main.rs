use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rust_code_analysis::{get_function_spaces, get_language_for_file, read_file_with_eol};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about = "Generate rust-code-analysis reports")]
struct Arguments {
    /// Paths to scan (files or directories).
    #[arg(long, short = 'p', value_parser, default_value = "crates")]
    paths: Vec<PathBuf>,
    /// Output directory for reports.
    #[arg(long, short = 'o', value_parser, default_value = "target/code-metrics")]
    output: PathBuf,
    /// Output format.
    #[arg(long, value_enum, default_value = "json")]
    format: OutputFormat,
    /// Pretty-print JSON output.
    #[arg(long)]
    pretty: bool,
    /// Write summary report after analysis.
    #[arg(long)]
    summary: bool,
    /// Only generate summary from existing outputs.
    #[arg(long)]
    summary_only: bool,
    /// Number of top results to include in summaries.
    #[arg(long, default_value_t = 20)]
    top: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum OutputFormat {
    Json,
    Toml,
}

impl OutputFormat {
    fn extension(self) -> &'static OsStr {
        match self {
            OutputFormat::Json => OsStr::new("json"),
            OutputFormat::Toml => OsStr::new("toml"),
        }
    }
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let workspace_root = std::env::current_dir().context("resolve workspace root")?;

    fs::create_dir_all(&arguments.output)
        .with_context(|| format!("create output directory {}", arguments.output.display()))?;
    let output_root = arguments
        .output
        .canonicalize()
        .unwrap_or_else(|_| arguments.output.clone());

    if arguments.summary_only {
        write_summary(&output_root, &workspace_root, arguments.top)?;
        return Ok(());
    }

    if arguments.summary && arguments.format != OutputFormat::Json {
        return Err(anyhow::anyhow!(
            "summary requires json output, use --format json"
        ));
    }

    let mut file_paths = Vec::new();
    for input_path in &arguments.paths {
        let canonical_input = input_path
            .canonicalize()
            .with_context(|| format!("resolve path {}", input_path.display()))?;
        if canonical_input.is_file() {
            if !canonical_input.starts_with(&output_root) {
                file_paths.push(canonical_input);
            }
            continue;
        }
        if !canonical_input.is_dir() {
            return Err(anyhow::anyhow!(
                "{} is not a file or directory",
                canonical_input.display()
            ));
        }

        for entry in WalkDir::new(&canonical_input) {
            let entry =
                entry.with_context(|| format!("walk directory {}", canonical_input.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.starts_with(&output_root) {
                continue;
            }
            file_paths.push(path.to_path_buf());
        }
    }

    for path in file_paths {
        let relative_path = path
            .strip_prefix(&workspace_root)
            .with_context(|| format!("path {} is outside the workspace", path.display()))?;

        let language = match get_language_for_file(&path) {
            Some(language) => language,
            None => continue,
        };

        let source =
            read_file_with_eol(&path).with_context(|| format!("read source {}", path.display()))?;
        let source = source
            .ok_or_else(|| anyhow::anyhow!("{} returned no source content", path.display()))?;
        let functions_space = get_function_spaces(&language, source, &path, None)
            .ok_or_else(|| anyhow::anyhow!("failed to compute metrics for {}", path.display()))?;

        let mut output_path = output_root.join(relative_path);
        output_path.set_extension(arguments.format.extension());
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create output directory {}", parent.display()))?;
        }

        match arguments.format {
            OutputFormat::Json => {
                let file = fs::File::create(&output_path)
                    .with_context(|| format!("write output {}", output_path.display()))?;
                if arguments.pretty {
                    serde_json::to_writer_pretty(file, &functions_space)
                        .context("serialize json")?;
                } else {
                    serde_json::to_writer(file, &functions_space).context("serialize json")?;
                }
            }
            OutputFormat::Toml => {
                let output = toml::to_string(&functions_space).context("serialize toml")?;
                fs::write(&output_path, output)
                    .with_context(|| format!("write output {}", output_path.display()))?;
            }
        }
    }

    if arguments.summary {
        write_summary(&output_root, &workspace_root, arguments.top)?;
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct SummaryReport {
    top_functions: TopFunctions,
    top_files: TopFiles,
    crates: Vec<CrateSummary>,
}

#[derive(Debug, Serialize)]
struct TopFunctions {
    cyclomatic: Vec<FunctionSummary>,
    cognitive: Vec<FunctionSummary>,
}

#[derive(Debug, Serialize)]
struct TopFiles {
    cyclomatic: Vec<FileSummary>,
    cognitive: Vec<FileSummary>,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionSummary {
    crate_name: String,
    file: String,
    function: String,
    start_line: u64,
    end_line: u64,
    cyclomatic: f64,
    cognitive: f64,
    sloc: f64,
}

#[derive(Debug, Serialize)]
struct CrateSummary {
    crate_name: String,
    functions: usize,
    average_cyclomatic: f64,
    average_cognitive: f64,
    max_cyclomatic: f64,
    max_cognitive: f64,
}

#[derive(Debug, Serialize, Clone)]
struct FileSummary {
    crate_name: String,
    file: String,
    functions: usize,
    average_cyclomatic: f64,
    average_cognitive: f64,
    max_cyclomatic: f64,
    max_cognitive: f64,
}

#[derive(Default)]
struct MetricTotals {
    count: usize,
    sum_cyclomatic: f64,
    sum_cognitive: f64,
    max_cyclomatic: f64,
    max_cognitive: f64,
}

impl MetricTotals {
    fn record(&mut self, cyclomatic: f64, cognitive: f64) {
        self.count += 1;
        self.sum_cyclomatic += cyclomatic;
        self.sum_cognitive += cognitive;
        if cyclomatic > self.max_cyclomatic {
            self.max_cyclomatic = cyclomatic;
        }
        if cognitive > self.max_cognitive {
            self.max_cognitive = cognitive;
        }
    }
}

fn write_summary(output_root: &PathBuf, workspace_root: &PathBuf, top: usize) -> Result<()> {
    let mut functions = Vec::new();
    let mut file_totals: HashMap<String, (String, MetricTotals)> = HashMap::new();
    let mut crate_totals: HashMap<String, MetricTotals> = HashMap::new();

    let mut found_json = false;
    for entry in WalkDir::new(output_root) {
        let entry = entry.with_context(|| format!("walk {}", output_root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension() != Some(OsStr::new("json")) {
            continue;
        }
        if entry.path().file_name() == Some(OsStr::new("summary.json")) {
            continue;
        }
        found_json = true;
        let content = fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        let value: Value = serde_json::from_str(&content).with_context(|| "parse json output")?;
        let file_path = extract_file_path(&value)
            .with_context(|| format!("extract file path from {}", entry.path().display()))?;
        let relative = file_path
            .strip_prefix(workspace_root)
            .unwrap_or(file_path.as_path());
        let file_string = relative.to_string_lossy().to_string();
        let crate_name = crate_name_from_path(relative);

        let mut file_functions = Vec::new();
        collect_functions(&value, &crate_name, &file_string, &mut file_functions)?;
        functions.extend(file_functions.iter().cloned());

        let totals = file_totals
            .entry(file_string.clone())
            .or_insert_with(|| (crate_name.clone(), MetricTotals::default()));
        let crate_totals_entry = crate_totals.entry(crate_name.clone()).or_default();
        for function in &file_functions {
            totals.1.record(function.cyclomatic, function.cognitive);
            crate_totals_entry.record(function.cyclomatic, function.cognitive);
        }
    }

    if !found_json {
        return Err(anyhow::anyhow!(
            "no json outputs found under {}",
            output_root.display()
        ));
    }

    let mut top_by_cyclomatic = functions.clone();
    top_by_cyclomatic.sort_by(|a, b| {
        b.cyclomatic
            .partial_cmp(&a.cyclomatic)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_by_cyclomatic.truncate(top);

    let mut top_by_cognitive = functions.clone();
    top_by_cognitive.sort_by(|a, b| {
        b.cognitive
            .partial_cmp(&a.cognitive)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_by_cognitive.truncate(top);

    let mut crate_summaries = Vec::new();
    for (crate_name, totals) in crate_totals {
        if totals.count == 0 {
            continue;
        }
        crate_summaries.push(CrateSummary {
            crate_name,
            functions: totals.count,
            average_cyclomatic: totals.sum_cyclomatic / totals.count as f64,
            average_cognitive: totals.sum_cognitive / totals.count as f64,
            max_cyclomatic: totals.max_cyclomatic,
            max_cognitive: totals.max_cognitive,
        });
    }

    let mut file_summaries = Vec::new();
    for (file, (crate_name, totals)) in file_totals {
        if totals.count == 0 {
            continue;
        }
        file_summaries.push(FileSummary {
            crate_name,
            file,
            functions: totals.count,
            average_cyclomatic: totals.sum_cyclomatic / totals.count as f64,
            average_cognitive: totals.sum_cognitive / totals.count as f64,
            max_cyclomatic: totals.max_cyclomatic,
            max_cognitive: totals.max_cognitive,
        });
    }

    let mut top_files_by_cyclomatic = file_summaries.clone();
    top_files_by_cyclomatic.sort_by(|a, b| {
        b.max_cyclomatic
            .partial_cmp(&a.max_cyclomatic)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_files_by_cyclomatic.truncate(top);

    let mut top_files_by_cognitive = file_summaries;
    top_files_by_cognitive.sort_by(|a, b| {
        b.max_cognitive
            .partial_cmp(&a.max_cognitive)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_files_by_cognitive.truncate(top);

    let report = SummaryReport {
        top_functions: TopFunctions {
            cyclomatic: top_by_cyclomatic,
            cognitive: top_by_cognitive,
        },
        top_files: TopFiles {
            cyclomatic: top_files_by_cyclomatic,
            cognitive: top_files_by_cognitive,
        },
        crates: crate_summaries,
    };

    let summary_path = output_root.join("summary.json");
    let file = fs::File::create(&summary_path)
        .with_context(|| format!("write summary {}", summary_path.display()))?;
    serde_json::to_writer_pretty(file, &report).context("serialize summary json")?;
    Ok(())
}

fn extract_file_path(value: &Value) -> Result<PathBuf> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing name field"))?;
    Ok(PathBuf::from(name))
}

fn collect_functions(
    value: &Value,
    crate_name: &str,
    file: &str,
    output: &mut Vec<FunctionSummary>,
) -> Result<()> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing kind field"))?;
    if kind == "function" {
        let function_name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing function name"))?
            .to_string();
        let start_line = value
            .get("start_line")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("missing start_line"))?;
        let end_line = value
            .get("end_line")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("missing end_line"))?;
        let metrics = value
            .get("metrics")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("missing metrics"))?;

        let cyclomatic = metric_sum(metrics, "cyclomatic")?;
        let cognitive = metric_sum(metrics, "cognitive")?;
        let sloc = metric_field(metrics, "loc", "sloc")?;

        output.push(FunctionSummary {
            crate_name: crate_name.to_string(),
            file: file.to_string(),
            function: function_name,
            start_line,
            end_line,
            cyclomatic,
            cognitive,
            sloc,
        });
    }

    let spaces = value
        .get("spaces")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing spaces"))?;
    for space in spaces {
        collect_functions(space, crate_name, file, output)?;
    }
    Ok(())
}

fn metric_sum(metrics: &serde_json::Map<String, Value>, key: &str) -> Result<f64> {
    let sum = metrics
        .get(key)
        .and_then(|value| value.get("sum"))
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("missing {}.sum", key))?;
    Ok(sum)
}

fn metric_field(metrics: &serde_json::Map<String, Value>, group: &str, field: &str) -> Result<f64> {
    let value = metrics
        .get(group)
        .and_then(|value| value.get(field))
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("missing {}.{}", group, field))?;
    Ok(value)
}

fn crate_name_from_path(path: &std::path::Path) -> String {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return "workspace".to_string();
    };
    if first.as_os_str() == OsStr::new("crates") {
        if let Some(second) = components.next() {
            return second.as_os_str().to_string_lossy().to_string();
        }
    }
    "workspace".to_string()
}
