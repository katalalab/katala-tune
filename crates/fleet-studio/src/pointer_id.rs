//! Stable, opaque element identities. Target text is never put in an element ID.
use std::collections::HashMap;

const MAX_IDENTITIES: usize = 4096;

#[derive(Default)]
pub struct PointerIds {
    saved: HashMap<Vec<String>, Vec<u64>>,
    seen: HashMap<Vec<String>, usize>,
    count: usize,
    next: u64,
}

impl PointerIds {
    pub fn begin_frame(&mut self) {
        self.seen.clear();
    }

    pub fn finish_frame(&mut self) {
        self.saved.retain(|key, ids| match self.seen.get(key) {
            Some(count) => {
                ids.truncate(*count);
                !ids.is_empty()
            }
            None => false,
        });
        self.count = self.saved.values().map(Vec::len).sum();
        self.seen.clear();
    }

    /// Invalidate pending clicks and immediately discard their private keys.
    pub fn clear(&mut self) {
        self.saved.clear();
        self.seen.clear();
        self.count = 0;
    }

    pub fn get(&mut self, kind: &str, parts: &[&str]) -> u64 {
        let key: Vec<String> = std::iter::once(kind).chain(parts.iter().copied()).map(str::to_owned).collect();
        let occurrence = self.seen.entry(key.clone()).or_default();
        let index = *occurrence;
        *occurrence += 1;
        if let Some(id) = self.saved.get(&key).and_then(|ids| ids.get(index)) {
            return *id;
        }
        self.next = self.next.checked_add(1).expect("pointer identity space exhausted");
        let id = self.next;
        if self.count < MAX_IDENTITIES {
            self.saved.entry(key).or_default().push(id);
            self.count += 1;
        }
        // Above the cap IDs are deliberately not reused, so a redraw cancels
        // an in-progress click rather than retargeting it to a different row.
        id
    }
}
