/* A single process handles successive queries and mode changes. */
use super::{QueryMode, SearchEngine};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

const HELP:&str="Usage: search --index <index-directory> [--mode and|or] [--dataset <TSV>] [--k1 <number>] [--b <number>] [--no-snippets]
Commands inside the prompt:
  /mode and       Require all query terms (conjunctive)
  /mode or        Match any query term (disjunctive; default)
  /and, /or       Shortcuts for changing mode
  /help           Show commands
  /quit           Exit (EOF also exits)
Enter ordinary text to run a query. The process stays open after each query.
";

pub fn run_cli(args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut args = args;
    let mut index = None;
    let mut dataset = None;
    let mut mode = QueryMode::Disjunctive;
    let mut snippets = true;
    let mut k1 = 1.2;
    let mut b = 0.75;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--index" => index = Some(PathBuf::from(value(&mut args, &arg)?)),
            "--dataset" => dataset = Some(PathBuf::from(value(&mut args, &arg)?)),
            "--mode" => mode = QueryMode::parse(&value(&mut args, &arg)?)?,
            "--k1" => k1 = number(&mut args, &arg)?,
            "--b" => b = number(&mut args, &arg)?,
            "--no-snippets" => snippets = false,
            _ => return Err(invalid(&format!("Unknown search option: {arg}\n{HELP}"))),
        }
    }
    let index = index.ok_or_else(|| invalid(HELP))?;
    let started = Instant::now();
    let mut engine = SearchEngine::open(index, dataset.as_deref(), snippets, k1, b)?;
    println!(
        "Loaded {} documents and {} terms in {:.2}s",
        engine.document_count(),
        engine.term_count(),
        started.elapsed().as_secs_f64()
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_session(&mut engine, &mut stdin.lock(), &mut stdout.lock(), mode)
}

pub fn run_session(
    engine: &mut SearchEngine,
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut mode: QueryMode,
) -> io::Result<()> {
    writeln!(
        output,
        "Search ready. Mode: {}. Use /mode and, /mode or, /help or /quit.",
        mode.label()
    )?;
    let mut line = String::new();
    loop {
        write!(output, "{}> ", mode.label())?;
        output.flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 {
            writeln!(output)?;
            break;
        }
        let query = line.trim();
        if query.is_empty() {
            continue;
        }
        if query.starts_with('/') {
            let parts: Vec<_> = query.split_whitespace().collect();
            match parts.as_slice() {
                ["/quit"] | ["/exit"] => break,
                ["/help"] => writeln!(output, "{HELP}")?,
                ["/and"] => {
                    mode = QueryMode::Conjunctive;
                    writeln!(output, "Mode: AND")?;
                }
                ["/or"] => {
                    mode = QueryMode::Disjunctive;
                    writeln!(output, "Mode: OR")?;
                }
                ["/mode", name] => match QueryMode::parse(name) {
                    Ok(next) => {
                        mode = next;
                        writeln!(output, "Mode: {}", mode.label())?;
                    }
                    Err(e) => writeln!(output, "{e}")?,
                },
                _ => writeln!(output, "Unknown command. Use /help for available commands.")?,
            }
            continue;
        }
        let started = Instant::now();
        let result = match engine.search(query, mode) {
            Ok(result) => result,
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                writeln!(output, "Query error: {e}")?;
                continue;
            }
            Err(e) => return Err(e),
        };
        writeln!(
            output,
            "Query: {query} | mode: {} | terms: {}",
            mode.label(),
            result.terms.join(", ")
        )?;
        if result.hits.is_empty() {
            writeln!(output, "No results.")?;
        }
        for (i, hit) in result.hits.iter().enumerate() {
            writeln!(
                output,
                "{}. passage_id={} doc_id={} score={:.6}",
                i + 1,
                hit.passage_id,
                hit.doc_id,
                hit.score
            )?;
            write!(output, "   terms: ")?;
            for (j, term) in hit.term_scores.iter().enumerate() {
                if j > 0 {
                    write!(output, ", ")?;
                }
                write!(
                    output,
                    "{}={:.6} (tf={})",
                    term.term, term.score, term.frequency
                )?;
            }
            writeln!(output)?;
            if !hit.snippet.is_empty() {
                writeln!(output, "   snippet: {}", hit.snippet)?;
            }
        }
        writeln!(
            output,
            "{} matching documents; showing {} results in {:.3}s\n",
            result.matched_documents,
            result.hits.len(),
            started.elapsed().as_secs_f64()
        )?;
    }
    Ok(())
}
fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<String> {
    args.next()
        .ok_or_else(|| invalid(&format!("Missing value for {flag}")))
}
fn number(args: &mut impl Iterator<Item = String>, flag: &str) -> io::Result<f64> {
    value(args, flag)?
        .parse()
        .map_err(|_| invalid(&format!("Invalid number for {flag}")))
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
