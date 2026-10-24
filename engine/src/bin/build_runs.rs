fn main() -> std::io::Result<()> {
    search_engine::app::run_build(std::env::args().skip(1))
}
