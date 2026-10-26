use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use indexify::{collection, model::Progress, ui};
use indexly::QueryMode;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Make your documents searchable.",
    after_help = "FORMATS  PDF · Markdown (.md) · Word (.docx)\n\nEXAMPLES\n  indexify ./documents --name research\n  indexify ./report.pdf --name report --no-ui\n  indexify search research \"inverted index\"\n  indexify browse research\n\nRun without arguments to open your library."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// A document or directory to index recursively.
    source: Option<PathBuf>,
    /// Collection name (defaults to the input filename).
    #[arg(long)]
    name: Option<String>,
    /// Build without launching the terminal interface.
    #[arg(long)]
    no_ui: bool,
    /// Collection storage location (also settable with INDEXIFY_HOME).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Search a collection and print its ten best passages.
    Search {
        collection: String,
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(long, default_value = "or", value_parser = ["and", "or"])]
        mode: String,
        #[arg(long)]
        json: bool,
    },
    /// Open the interactive document library.
    Browse { collection: Option<String> },
    /// List indexed collections.
    List,
    /// Rebuild a collection from its original source, safely replacing its snapshot.
    Refresh {
        collection: String,
        #[arg(long)]
        no_ui: bool,
    },
    /// Remove a collection's index and snapshots. Original documents are untouched.
    Remove { collection: String },
    /// Export a real terminal frame as an SVG, without needing a TTY.
    Preview {
        collection: String,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value = "indexify-preview.svg")]
        output: PathBuf,
        #[arg(long, default_value_t = 144, value_parser = clap::value_parser!(u16).range(40..=240))]
        width: u16,
        #[arg(long, default_value_t = 42, value_parser = clap::value_parser!(u16).range(16..=100))]
        height: u16,
    },
}

fn progress(item: Progress) -> Result<()> {
    if item.total > 0 {
        eprintln!(
            "  {} [{}/{}] {}",
            item.stage,
            item.done,
            item.total,
            collection::safe_text(&item.detail)
        );
    } else {
        eprintln!("  {} · {}", item.stage, collection::safe_text(&item.detail));
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("indexify: {}", collection::safe_text(&format!("{error:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(collection::default_home);
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    match cli.command {
        Some(Command::Search {
            collection: name,
            query,
            mode,
            json,
        }) => {
            let mut library = collection::Collection::open(&home, &name)?;
            let result = library.search(&query.join(" "), QueryMode::parse(&mode)?)?;
            if json {
                serde_json::to_writer_pretty(io::stdout().lock(), &result)?;
                println!();
            } else {
                println!(
                    "{} · {} matching passages · {} ms\n",
                    name, result.matched_passages, result.elapsed_ms
                );
                for hit in result.hits {
                    println!(
                        "{}. {} · {} · {:.3}",
                        hit.rank,
                        collection::safe_text(&hit.passage.source.display().to_string()),
                        hit.passage.location,
                        hit.score
                    );
                    if !hit.passage.heading.is_empty() {
                        println!("   {}", hit.passage.heading);
                    }
                    println!("   {}\n", hit.text);
                }
                if result.matched_passages == 0 {
                    println!("No matches. Try fewer words or --mode or.");
                }
            }
        }
        Some(Command::Browse { collection: name }) => {
            require_terminal(interactive)?;
            ui::run(home, ui::Launch::Browse(name))?;
        }
        Some(Command::List) => {
            let libraries = collection::list(&home)?;
            if libraries.is_empty() {
                println!(
                    "Your library is empty. Run: indexify <document-or-directory> --name <name>"
                );
            }
            for library in libraries {
                println!(
                    "{}\t{} documents\t{} passages\t{}",
                    library.name,
                    library.files,
                    library.passages,
                    collection::safe_text(&library.source.display().to_string())
                );
            }
        }
        Some(Command::Refresh {
            collection: name,
            no_ui,
        }) => {
            if interactive && !no_ui {
                ui::run(home, ui::Launch::Refresh(name))?;
            } else {
                let built = collection::refresh(&home, &name, &mut progress)?;
                report(&built);
            }
        }
        Some(Command::Remove { collection: name }) => {
            collection::remove(&home, &name)?;
            println!("Removed collection '{name}'. Original documents are unchanged.");
        }
        Some(Command::Preview {
            collection: name,
            query,
            output,
            width,
            height,
        }) => {
            ui::snapshot(&home, &name, &query, &output, width, height)?;
            println!("Preview: {}", output.display());
        }
        None => {
            if let Some(source) = cli.source {
                let name = cli
                    .name
                    .unwrap_or_else(|| collection::suggested_name(&source));
                if interactive && !cli.no_ui {
                    ui::run(home, ui::Launch::Build { source, name })?;
                } else {
                    let built = collection::build(&home, &name, &source, false, &mut progress)?;
                    report(&built);
                }
            } else {
                if cli.name.is_some() || cli.no_ui {
                    bail!("Provide a document or directory to index");
                }
                require_terminal(interactive)?;
                ui::run(home, ui::Launch::Browse(None))?;
            }
        }
    }
    Ok(())
}

fn require_terminal(interactive: bool) -> Result<()> {
    if !interactive {
        bail!(
            "The interface needs an interactive terminal. Use 'indexify search <collection> <query>' or 'indexify --help' for scriptable commands"
        );
    }
    Ok(())
}

fn report(built: &indexify::model::Manifest) {
    println!(
        "\nIndexed '{}' · {} documents · {} passages",
        built.name, built.files, built.passages
    );
    for issue in &built.issues {
        eprintln!(
            "Skipped {}: {}",
            collection::safe_text(&issue.source.display().to_string()),
            collection::safe_text(&issue.message)
        );
    }
    println!("Search: indexify search {} <words>", built.name);
    println!("Browse: indexify browse {}", built.name);
}
