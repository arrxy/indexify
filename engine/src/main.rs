use std::io;
fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("search") => search_engine::retrieval::run_cli(args),
        Some("prepare-snippets") => search_engine::app::run_prepare(args),
        _ => search_engine::app::run_index(std::env::args().skip(1)),
    }
}
