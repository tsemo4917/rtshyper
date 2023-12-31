use alloc::vec::Vec;

pub trait CacheInfoTrait {
    /// Get how many levels of cache there are in the system
    fn init_cache_level() -> usize;

    fn get_cache_info(level: usize) -> Self;

    fn level(&self) -> usize;

    fn num_sets(&self) -> usize;

    fn size(&self) -> usize;

    /// ways of associativity
    fn ways(&self) -> usize;

    fn line_size(&self) -> usize;

    fn num_colors(&self) -> usize;
}

#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum CacheType {
    #[default]
    NoCache,
    Instruction,
    Data,
    Separate,
    Unified,
}

#[derive(Copy, Clone, Default)]
pub enum CacheIndexed {
    #[default]
    Pipt,
    Vipt,
}

pub struct CpuCacheInfo<T: CacheInfoTrait> {
    pub info_list: Vec<T>,
    pub min_share_level: usize,
    pub num_levels: usize,
    pub _num_leaves: usize,
}
