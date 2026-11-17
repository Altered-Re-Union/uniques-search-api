use crate::card::CardJson;
use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use std::fs;
use std::path::Path;

/// Three independent boolean flags (a card can be any combination, unlike faction/rarity/product
/// which partition the set) — `IsBanned`/`IsErrated`/`IsSuspended`, present on both unique and
/// non-unique CardsData rows but not previously read anywhere.
#[derive(Debug, Default)]
pub struct StatusFlagsBuilder {
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl StatusFlagsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, card_index: u32, card: &CardJson) {
        if card.is_banned {
            self.banned.insert(card_index);
        }
        if card.is_errated {
            self.errated.insert(card_index);
        }
        if card.is_suspended {
            self.suspended.insert(card_index);
        }
    }

    pub fn into_flags(self) -> StatusFlags {
        StatusFlags {
            banned: self.banned,
            errated: self.errated,
            suspended: self.suspended,
        }
    }
}

pub struct StatusFlags {
    banned: RoaringBitmap,
    errated: RoaringBitmap,
    suspended: RoaringBitmap,
}

impl StatusFlags {
    /// Writes non-empty bitmaps under `status/{banned,errated,suspended}.roar`.
    pub fn write_dir(&self, status_root: &Path) -> Result<()> {
        fs::create_dir_all(status_root)?;
        for (name, bitmap) in [
            ("banned", &self.banned),
            ("errated", &self.errated),
            ("suspended", &self.suspended),
        ] {
            if bitmap.is_empty() {
                continue;
            }
            let path = status_root.join(format!("{name}.roar"));
            let mut bytes = Vec::new();
            bitmap
                .serialize_into(&mut bytes)
                .with_context(|| format!("serialize status flag {name}"))?;
            fs::write(&path, bytes)?;
        }
        Ok(())
    }
}
