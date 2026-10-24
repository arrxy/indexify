/* Sort a posting batch before it is written as a run. GPU is the default
when compiled in and uses batch-local lexicographic numeric term IDs.
CPU sorting is available through --sort cpu or a CPU-only build. */
use std::io;

use super::builder::IntermediatePosting;

mod cpu;

#[cfg(feature = "gpu-sort")]
mod gpu;

pub use cpu::CpuSorter;

// CPU sorting leaves the batch ordered in place. GPU sorting instead
// returns original indices in sorted order, leaving String ownership alone.
pub enum PostingOrder {
    InPlace,
    #[cfg(any(feature = "gpu-sort", test))]
    Indices(Vec<u32>),
}

pub trait PostingSorter: Sync {
    fn sort(&self, postings: &mut [IntermediatePosting]) -> io::Result<PostingOrder>;
    fn description(&self) -> &str;
}

pub fn create_sorter(mode: &str) -> io::Result<Box<dyn PostingSorter>> {
    match mode {
        "cpu" => Ok(Box::new(CpuSorter)),
        #[cfg(feature = "gpu-sort")]
        "gpu" => Ok(Box::new(gpu::GpuSorter::new()?)),
        #[cfg(not(feature = "gpu-sort"))]
        "gpu" => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "GPU sorting requires rebuilding with: cargo build --release --features gpu-sort",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--sort must be cpu or gpu",
        )),
    }
}
