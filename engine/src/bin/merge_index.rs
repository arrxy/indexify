fn main() -> std::io::Result<()> {
    search_engine::app::run_merge(std::env::args().skip(1))
}
