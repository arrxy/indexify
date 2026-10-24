fn main() -> std::io::Result<()> {
    search_engine::retrieval::run_cli(std::env::args().skip(1))
}
