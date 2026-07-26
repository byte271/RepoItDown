use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use repoitdown_core::Pipeline;

#[derive(Parser)]
#[command(
    version,
    about = "AST-aware codebase topology for LLM context windows",
    long_about = "Transforms repositories into token-optimized Markdown topologies.\n\
                  Uses tree-sitter AST parsing for Rust, Python, TypeScript, and Go;\n\
                  falls back to regex skeletonization for all other languages."
)]
struct Cli {
    /// Path to the repository to analyze
    path: PathBuf,

    /// Processing mode: dump, explore, architect, or task.
    #[arg(short, long, default_value = "dump", value_parser = ["dump", "explore", "architect", "task"])]
    mode: String,

    /// Maximum output tokens. For architect/task also the slicing budget.
    #[arg(long)]
    max_tokens: Option<usize>,

    /// Write output to a file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Disable collapsible HTML details (plain Markdown)
    #[arg(long)]
    no_collapse: bool,

    /// Free-text query for task mode. Required when --mode task.
    #[arg(long)]
    query: Option<String>,

    /// Print verbose diagnostics to stderr
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let _ = tracing_subscriber::fmt()
        .with_max_level(if cli.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::WARN
        })
        .with_writer(std::io::stderr)
        .try_init();

    let mut pipeline = Pipeline::new();
    if let Err(msg) = pipeline.configure(&cli.mode, cli.query.as_deref(), cli.max_tokens, !cli.no_collapse) {
        eprintln!("error: {msg}");
        return ExitCode::FAILURE;
    }

    match pipeline.run(&cli.path) {
        Ok(output) => {
            if let Some(out_path) = &cli.output {
                if let Err(e) = std::fs::write(out_path, &output) {
                    eprintln!("error: failed to write {}: {e}", out_path.display());
                    return ExitCode::FAILURE;
                }
            } else {
                std::io::stdout().lock().write_all(output.as_bytes()).ok();
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
