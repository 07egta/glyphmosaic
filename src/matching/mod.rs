pub mod scan;
pub mod simd;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct Query {
    pub full: [u8; 256],
    pub coarse: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchResult {
    pub index: usize,
    pub mse: u32,
}
