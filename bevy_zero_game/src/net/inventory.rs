//! Complete, paged inventory snapshots for the replacement UDP transport.

use std::collections::BTreeMap;

use super::protocol::NetInventoryItemDto;

/// Serial-number comparison remains valid across a wrapping 32-bit revision counter.
pub(super) fn revision_is_newer(candidate: u32, current: u32) -> bool {
    candidate != current && candidate.wrapping_sub(current) < (1 << 31)
}

#[derive(Debug, Default)]
pub(super) struct InventoryAssembler {
    committed_revision: Option<u32>,
    pending: Option<PendingInventory>,
}

#[derive(Debug)]
struct PendingInventory {
    revision: u32,
    pages: Vec<Option<Vec<NetInventoryItemDto>>>,
}

impl InventoryAssembler {
    pub fn committed_revision(&self) -> Option<u32> {
        self.committed_revision
    }
    /// Publishes only a complete snapshot, so missing/reordered UDP pages cannot erase items.
    pub fn receive(
        &mut self,
        revision: u32,
        page_index: u16,
        page_count: u16,
        items: Vec<NetInventoryItemDto>,
    ) -> Option<BTreeMap<i32, i32>> {
        if page_count == 0 || page_index >= page_count || page_count > 256 {
            return None;
        }
        if self
            .committed_revision
            .is_some_and(|current| !revision_is_newer(revision, current))
        {
            return None;
        }
        if let Some(pending) = &self.pending {
            if revision != pending.revision && !revision_is_newer(revision, pending.revision) {
                return None;
            }
            if revision == pending.revision && pending.pages.len() != usize::from(page_count) {
                return None;
            }
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.revision != revision)
        {
            self.pending = Some(PendingInventory {
                revision,
                pages: vec![None; usize::from(page_count)],
            });
        }
        let pending = self.pending.as_mut()?;
        let page = &mut pending.pages[usize::from(page_index)];
        if page.as_ref().is_some_and(|previous| previous != &items) {
            // A revision denotes one immutable inventory; conflicting duplicates invalidate it.
            self.pending = None;
            return None;
        }
        *page = Some(items);
        if pending.pages.iter().any(Option::is_none) {
            return None;
        }
        let mut counts = BTreeMap::new();
        for item in pending.pages.iter().filter_map(Option::as_ref).flatten() {
            if item.item_id <= 0 || item.num < 0 || counts.insert(item.item_id, item.num).is_some()
            {
                self.pending = None;
                return None;
            }
        }
        counts.retain(|_, num| *num > 0);
        self.committed_revision = Some(revision);
        self.pending = None;
        Some(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::protocol::{PacketEnvelope, PacketKind, decode_packet, encode_packet};

    fn item(id: i32) -> NetInventoryItemDto {
        NetInventoryItemDto {
            item_id: id,
            num: id,
        }
    }
    fn full_page(first: i32) -> Vec<NetInventoryItemDto> {
        (first..first + 128).map(item).collect()
    }
    fn wire_receive(
        assembler: &mut InventoryAssembler,
        revision: u32,
        page_index: u16,
        page_count: u16,
        items: Vec<NetInventoryItemDto>,
    ) -> Option<BTreeMap<i32, i32>> {
        let wire = encode_packet(&PacketEnvelope {
            seq: 1,
            kind: PacketKind::InventorySnapshot {
                revision,
                page_index,
                page_count,
                items,
            },
        });
        assert!(wire.len() < 1100);
        let PacketKind::InventorySnapshot {
            revision,
            page_index,
            page_count,
            items,
        } = decode_packet(&wire).unwrap().kind
        else {
            panic!("inventory snapshot");
        };
        assembler.receive(revision, page_index, page_count, items)
    }

    #[test]
    fn reordered_pages_publish_together_and_old_revisions_never_erase_items() {
        let mut assembler = InventoryAssembler::default();
        assert!(wire_receive(&mut assembler, 10, 1, 2, vec![item(57100)]).is_none());
        assert!(wire_receive(&mut assembler, 9, 0, 1, vec![item(50001)]).is_none());
        let counts = wire_receive(&mut assembler, 10, 0, 2, full_page(50003)).unwrap();
        assert_eq!(counts.len(), 129);
        assert_eq!(counts[&57100], 57100);
        assert!(wire_receive(&mut assembler, 10, 0, 1, vec![]).is_none());
        assert!(wire_receive(&mut assembler, 9, 0, 1, vec![]).is_none());
        assert_eq!(
            wire_receive(&mut assembler, 11, 0, 1, vec![]).unwrap(),
            BTreeMap::new()
        );
    }

    #[test]
    fn duplicate_ids_and_conflicting_pages_do_not_publish_partial_inventory() {
        let mut assembler = InventoryAssembler::default();
        assert!(wire_receive(&mut assembler, 1, 0, 2, full_page(50001)).is_none());
        assert!(wire_receive(&mut assembler, 1, 1, 2, vec![item(50001)]).is_none());
        assert!(wire_receive(&mut assembler, 2, 0, 2, full_page(50001)).is_none());
        assert!(wire_receive(&mut assembler, 2, 0, 2, full_page(50003)).is_none());
        assert!(wire_receive(&mut assembler, 2, 1, 2, vec![item(50006)]).is_none());
        assert!(wire_receive(&mut assembler, 3, 256, 257, vec![item(50006)]).is_none());
    }

    #[test]
    fn zero_counts_from_valid_wire_pages_commit_as_absent_items() {
        let mut assembler = InventoryAssembler::default();
        let counts = wire_receive(
            &mut assembler,
            1,
            0,
            1,
            vec![
                NetInventoryItemDto {
                    item_id: 50001,
                    num: 0,
                },
                item(50003),
            ],
        )
        .unwrap();
        assert!(!counts.contains_key(&50001));
        assert_eq!(counts[&50003], 50003);
    }

    #[test]
    fn revision_order_crosses_u32_wrap() {
        let mut assembler = InventoryAssembler::default();
        assert!(wire_receive(&mut assembler, u32::MAX, 0, 1, vec![item(50001)]).is_some());
        assert!(wire_receive(&mut assembler, 0, 0, 1, vec![item(50003)]).is_some());
        assert!(wire_receive(&mut assembler, u32::MAX, 0, 1, vec![]).is_none());
    }
}
