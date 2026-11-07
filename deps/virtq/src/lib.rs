//! Virtio split virtqueue descriptor chain walking, including
//! `VIRTIO_RING_F_INDIRECT_DESC` (indirect descriptor) support.
//!
//! This crate is pure `no_std` logic with no dependency on the hypervisor,
//! so it can be unit-tested on the host. The hypervisor provides the raw
//! descriptor table and a resolver closure that borrows an indirect descriptor
//! table in guest memory. Walking a chain allocates nothing.

#![no_std]

#[cfg(test)]
extern crate alloc;

use core::mem::size_of;

/// This marks a buffer as continuing via the next field.
pub const VIRTQ_DESC_F_NEXT: u16 = 1;
/// This marks a buffer as write-only (otherwise read-only).
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
/// This means the buffer contains a table of descriptors.
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Feature bit: the driver can use descriptors with the `VIRTQ_DESC_F_INDIRECT`
/// flag set, pointing to a table of descriptors anywhere in memory.
pub const VIRTIO_RING_F_INDIRECT_DESC: usize = 1 << 28;

/// Virtio split-ring descriptor (16 bytes), layout compatible with the spec.
#[repr(C, align(16))]
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct VringDesc {
    /// Address (guest-physical).
    pub addr: u64,
    /// Length.
    pub len: u32,
    /// The flags as indicated above (`VIRTQ_DESC_F_*`).
    pub flags: u16,
    /// Next descriptor index if `VIRTQ_DESC_F_NEXT` is set.
    pub next: u16,
}

/// One descriptor yielded by [`DescIter`].
///
/// An indirect head descriptor is never yielded (its write-only flag is
/// meaningless per spec); the iterator yields the descriptors stored in the
/// indirect table instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescInfo {
    /// Address (guest-physical).
    pub addr: u64,
    /// Length.
    pub len: u32,
    /// Descriptor flags (`VIRTQ_DESC_F_NEXT` / `VIRTQ_DESC_F_WRITE`).
    pub flags: u16,
}

impl DescInfo {
    /// Whether the buffer described is writable by the device.
    pub fn writable(&self) -> bool {
        self.flags & VIRTQ_DESC_F_WRITE != 0
    }
}

/// Errors that can occur while walking a descriptor chain.
#[derive(Debug, PartialEq, Eq)]
pub enum ChainError {
    /// The chain head index is out of the descriptor table bounds.
    HeadOutOfBounds,
    /// A `next` index is out of the (indirect) descriptor table bounds.
    NextOutOfBounds,
    /// The head descriptor has both INDIRECT and NEXT set, which the
    /// driver MUST NOT do (spec 2.7.5.1 / 2.8.5.1).
    IndirectWithNext,
    /// The indirect table length is zero, not a multiple of 16, or larger
    /// than the queue size (hardening against malicious guests).
    BadIndirectLen,
    /// The indirect table could not be resolved (e.g. invalid guest address).
    IndirectTableUnavailable,
    /// A descriptor inside an indirect table has INDIRECT set, which the
    /// driver MUST NOT do (nested indirect tables are forbidden).
    NestedIndirect,
    /// The chain is longer than the descriptor table, i.e. it contains a loop.
    ChainTooLong,
}

/// Iterator over one descriptor chain, direct or indirect.
///
/// Each `next` reads a single descriptor from the underlying table. The
/// indirect head itself is not part of the iteration. `Item` is `Result` so a
/// broken link (`next` out of bounds, a loop, a nested indirect descriptor)
/// surfaces at the step where it is found; earlier descriptors stay usable,
/// which lets a device stop once it has enough buffers.
pub struct DescIter<'a> {
    table: &'a [VringDesc],
    idx: usize,
    walked: usize,
    indirect: bool,
    exhausted: bool,
}

impl<'a> Iterator for DescIter<'a> {
    type Item = Result<DescInfo, ChainError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted {
            return None;
        }
        let Some(desc) = self.table.get(self.idx).copied() else {
            self.exhausted = true;
            return Some(Err(ChainError::NextOutOfBounds));
        };
        if self.indirect && desc.flags & VIRTQ_DESC_F_INDIRECT != 0 {
            self.exhausted = true;
            return Some(Err(ChainError::NestedIndirect));
        }
        self.walked += 1;
        // A chain can hold at most `table.len()` descriptors; one more hop
        // means the driver created a loop.
        if self.walked > self.table.len() {
            self.exhausted = true;
            return Some(Err(ChainError::ChainTooLong));
        }
        let info = DescInfo {
            addr: desc.addr,
            len: desc.len,
            flags: desc.flags,
        };
        if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
            self.exhausted = true;
        } else {
            self.idx = desc.next as usize;
        }
        Some(Ok(info))
    }
}

impl<'a> core::iter::FusedIterator for DescIter<'a> {}

/// Start iterating the descriptor chain at index `head` of `table`.
///
/// Structural problems of the head (out of bounds, INDIRECT combined with
/// NEXT, a bad indirect length, an unresolvable indirect table) are returned
/// immediately. Link errors inside the chain are yielded by [`DescIter`].
///
/// `table.len()` is the queue size: it bounds every index hop and caps the
/// indirect table entry count. `resolve_indirect(gpa, len)` must borrow the
/// indirect table in place; it is not copied.
pub fn iter_chain<'a, F>(table: &'a [VringDesc], head: u16, resolve_indirect: F) -> Result<DescIter<'a>, ChainError>
where
    F: FnOnce(u64, u32) -> Option<&'a [VringDesc]>,
{
    // Copy the head before inspecting it. Another guest vCPU can write the
    // descriptor table while the device walks it; addr, len and flags must
    // come from one snapshot.
    let head_desc = *table.get(head as usize).ok_or(ChainError::HeadOutOfBounds)?;

    if head_desc.flags & VIRTQ_DESC_F_INDIRECT == 0 {
        return Ok(DescIter {
            table,
            idx: head as usize,
            walked: 0,
            indirect: false,
            exhausted: false,
        });
    }

    // The driver MUST NOT set both VIRTQ_DESC_F_INDIRECT and VIRTQ_DESC_F_NEXT.
    if head_desc.flags & VIRTQ_DESC_F_NEXT != 0 {
        return Err(ChainError::IndirectWithNext);
    }
    let len = head_desc.len as usize;
    let count = len / size_of::<VringDesc>();
    if len == 0 || len % size_of::<VringDesc>() != 0 || count > table.len() {
        return Err(ChainError::BadIndirectLen);
    }
    let indirect = resolve_indirect(head_desc.addr, head_desc.len).ok_or(ChainError::IndirectTableUnavailable)?;
    if indirect.len() < count {
        return Err(ChainError::IndirectTableUnavailable);
    }
    Ok(DescIter {
        table: &indirect[..count],
        idx: 0,
        walked: 0,
        indirect: true,
        exhausted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn desc(addr: u64, len: u32, flags: u16, next: u16) -> VringDesc {
        VringDesc { addr, len, flags, next }
    }

    fn no_indirect<'a>(_gpa: u64, _len: u32) -> Option<&'a [VringDesc]> {
        None
    }

    /// Borrows the first `len / 16` entries of `table`, the same way the
    /// hypervisor translates an indirect table IPA and walks it in place.
    fn resolver<'a>(table: &'a [VringDesc]) -> impl FnOnce(u64, u32) -> Option<&'a [VringDesc]> + 'a {
        move |_gpa, len| {
            let n = len as usize / size_of::<VringDesc>();
            if n > table.len() {
                return None;
            }
            Some(&table[..n])
        }
    }

    /// Test helper: collect a whole chain. Production code iterates instead.
    fn walk_chain<'a, F>(table: &'a [VringDesc], head: u16, resolve: F) -> Result<Vec<DescInfo>, ChainError>
    where
        F: FnOnce(u64, u32) -> Option<&'a [VringDesc]>,
    {
        iter_chain(table, head, resolve)?.collect()
    }

    #[test]
    fn single_direct_desc() {
        let table = vec![desc(0x1000, 512, 0, 0)];
        let chain = walk_chain(&table, 0, no_indirect).unwrap();
        assert_eq!(
            chain,
            vec![DescInfo {
                addr: 0x1000,
                len: 512,
                flags: 0
            }]
        );
    }

    #[test]
    fn direct_chain_follows_next_in_order() {
        let table = vec![
            desc(0x1000, 16, VIRTQ_DESC_F_NEXT, 2),
            desc(0x2000, 32, 0, 0),
            desc(0x3000, 64, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 1),
        ];
        let chain = walk_chain(&table, 0, no_indirect).unwrap();
        let addrs: Vec<u64> = chain.iter().map(|d| d.addr).collect();
        assert_eq!(addrs, vec![0x1000, 0x3000, 0x2000]);
        assert!(chain[1].writable());
        assert!(!chain[0].writable());
    }

    #[test]
    fn direct_chain_uses_whole_table() {
        // 0 -> 1 -> 2, last has no NEXT: exactly table.len() hops, legal.
        let table = vec![
            desc(0x1000, 1, VIRTQ_DESC_F_NEXT, 1),
            desc(0x2000, 1, VIRTQ_DESC_F_NEXT, 2),
            desc(0x3000, 1, 0, 0),
        ];
        assert_eq!(walk_chain(&table, 0, no_indirect).unwrap().len(), 3);
    }

    #[test]
    fn direct_chain_loop_detected() {
        let table = vec![
            desc(0x1000, 16, VIRTQ_DESC_F_NEXT, 1),
            desc(0x2000, 16, VIRTQ_DESC_F_NEXT, 0),
        ];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::ChainTooLong));
    }

    #[test]
    fn direct_next_out_of_bounds() {
        let table = vec![desc(0x1000, 16, VIRTQ_DESC_F_NEXT, 7)];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::NextOutOfBounds));
    }

    #[test]
    fn iter_can_stop_before_a_broken_tail() {
        // Receiving paths stop once the buffers cover the frame. A bad link
        // further down the chain must not be reported if it is never visited.
        let table = vec![
            desc(0x1000, 16, VIRTQ_DESC_F_NEXT, 1),
            desc(0x2000, 16, VIRTQ_DESC_F_NEXT, 99),
        ];
        let mut iter = iter_chain(&table, 0, no_indirect).unwrap();
        assert_eq!(iter.next().unwrap().unwrap().addr, 0x1000);
    }

    #[test]
    fn head_out_of_bounds() {
        let table = vec![desc(0x1000, 16, 0, 0)];
        assert_eq!(walk_chain(&table, 5, no_indirect), Err(ChainError::HeadOutOfBounds));
    }

    #[test]
    fn empty_table_rejected() {
        let table: Vec<VringDesc> = vec![];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::HeadOutOfBounds));
    }

    #[test]
    fn indirect_basic_chain_flattened() {
        let indirect = [
            desc(0x5000, 16, VIRTQ_DESC_F_NEXT, 1),
            desc(0x6000, 512, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 2),
            desc(0x7000, 1, VIRTQ_DESC_F_WRITE, 0),
        ];
        let table = vec![
            desc(0x4000, (3 * size_of::<VringDesc>()) as u32, VIRTQ_DESC_F_INDIRECT, 0),
            desc(0xdead, 8, 0, 0),
            desc(0xdead, 8, 0, 0),
            desc(0xdead, 8, 0, 0),
        ];
        let chain = walk_chain(&table, 0, resolver(&indirect)).unwrap();
        // The flattened chain must not contain the indirect head itself.
        assert_eq!(
            chain,
            vec![
                DescInfo {
                    addr: 0x5000,
                    len: 16,
                    flags: VIRTQ_DESC_F_NEXT
                },
                DescInfo {
                    addr: 0x6000,
                    len: 512,
                    flags: VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
                },
                DescInfo {
                    addr: 0x7000,
                    len: 1,
                    flags: VIRTQ_DESC_F_WRITE
                },
            ]
        );
    }

    #[test]
    fn indirect_with_next_rejected() {
        let table = vec![
            desc(0x4000, 16, VIRTQ_DESC_F_INDIRECT | VIRTQ_DESC_F_NEXT, 1),
            desc(0x5000, 1, 0, 0),
        ];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::IndirectWithNext));
    }

    #[test]
    fn indirect_bad_len_rejected() {
        // len not a multiple of 16
        let table = vec![desc(0x4000, 17, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::BadIndirectLen));
        // zero len
        let table = vec![desc(0x4000, 0, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::BadIndirectLen));
        // indirect table larger than the queue size
        let table = vec![desc(0x4000, 32, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(walk_chain(&table, 0, no_indirect), Err(ChainError::BadIndirectLen));
    }

    #[test]
    fn indirect_table_unavailable() {
        let table = vec![desc(0xdead_0000, 16, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(
            walk_chain(&table, 0, no_indirect),
            Err(ChainError::IndirectTableUnavailable)
        );
    }

    #[test]
    fn indirect_short_resolve_rejected() {
        // Resolver hands back fewer descriptors than the head len claims.
        let indirect = [desc(0x5000, 16, 0, 0)];
        let table = vec![desc(0x4000, 32, VIRTQ_DESC_F_INDIRECT, 0), desc(0, 0, 0, 0)];
        assert_eq!(
            walk_chain(&table, 0, resolver(&indirect)),
            Err(ChainError::IndirectTableUnavailable)
        );
    }

    #[test]
    fn indirect_nested_rejected() {
        let indirect = [desc(0x5000, 16, VIRTQ_DESC_F_INDIRECT, 0)];
        let table = vec![desc(0x4000, 16, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(
            walk_chain(&table, 0, resolver(&indirect)),
            Err(ChainError::NestedIndirect)
        );
    }

    #[test]
    fn indirect_next_out_of_bounds() {
        let indirect = [desc(0x5000, 16, VIRTQ_DESC_F_NEXT, 9)];
        let table = vec![desc(0x4000, 16, VIRTQ_DESC_F_INDIRECT, 0)];
        assert_eq!(
            walk_chain(&table, 0, resolver(&indirect)),
            Err(ChainError::NextOutOfBounds)
        );
    }

    #[test]
    fn indirect_loop_detected() {
        let indirect = [
            desc(0x5000, 16, VIRTQ_DESC_F_NEXT, 1),
            desc(0x6000, 16, VIRTQ_DESC_F_NEXT, 0),
        ];
        let table = vec![desc(0x4000, 32, VIRTQ_DESC_F_INDIRECT, 0), desc(0, 0, 0, 0)];
        assert_eq!(
            walk_chain(&table, 0, resolver(&indirect)),
            Err(ChainError::ChainTooLong)
        );
    }

    #[test]
    fn indirect_head_write_flag_ignored() {
        // The device MUST ignore the write-only flag of the descriptor that
        // refers to an indirect table; only table entries are returned.
        let indirect = [desc(0x5000, 16, 0, 0)];
        let table = vec![desc(0x4000, 16, VIRTQ_DESC_F_INDIRECT | VIRTQ_DESC_F_WRITE, 0)];
        let chain = walk_chain(&table, 0, resolver(&indirect)).unwrap();
        assert_eq!(
            chain,
            vec![DescInfo {
                addr: 0x5000,
                len: 16,
                flags: 0
            }]
        );
    }

    #[test]
    fn desc_info_writable() {
        assert!(DescInfo {
            addr: 0,
            len: 0,
            flags: VIRTQ_DESC_F_WRITE
        }
        .writable());
        assert!(!DescInfo {
            addr: 0,
            len: 0,
            flags: VIRTQ_DESC_F_NEXT
        }
        .writable());
        assert!(!DescInfo {
            addr: 0,
            len: 0,
            flags: 0
        }
        .writable());
    }
}
