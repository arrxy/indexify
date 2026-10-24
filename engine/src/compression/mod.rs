use std::io;

mod simple9;
pub use simple9::Simple9;

pub trait IntegerCodec {
    fn id(&self) -> u8;
    fn encode(&self, values: &[u32]) -> io::Result<Vec<u8>>;
    fn decode(&self, bytes: &[u8], value_count: usize) -> io::Result<Vec<u32>>;
}
