use std::convert::TryInto;
use std::sync::Arc;
use std::time::Duration;

use mini_moka::sync::Cache;
use roaring::RoaringBitmap;

use crate::config::CollectionsSettings;

/// A named collection resolves to one bitmap per card kind — a collection can mix unique and
/// non-unique references (see `docs/non-unique-refonte-decisions.md`), and the two live in
/// separate bit spaces just like the rest of the unique/non-unique split.
#[derive(Debug, Default, Clone)]
pub struct CollectionBitmaps {
    pub unique: RoaringBitmap,
    pub nonunique: RoaringBitmap,
}

impl CollectionBitmaps {
    fn serialized_size(&self) -> usize {
        self.unique.serialized_size() + self.nonunique.serialized_size()
    }
}

#[derive(Clone)]
pub struct CollectionStore {
    cache: Cache<String, Arc<CollectionBitmaps>>,
}

impl CollectionStore {
    pub fn new(settings: &CollectionsSettings) -> Self {
        let mut builder = Cache::builder()
            .weigher(|key: &String, value: &Arc<CollectionBitmaps>| -> u32 {
                let key_weight = key.len();
                let bitmap_weight = value.serialized_size();
                (key_weight + bitmap_weight)
                    .try_into()
                    .unwrap_or(u32::MAX)
            })
            .max_capacity(settings.max_memory_bytes);

        if settings.time_to_live_secs > 0 {
            builder = builder.time_to_live(Duration::from_secs(settings.time_to_live_secs));
        }
        if settings.time_to_idle_secs > 0 {
            builder = builder.time_to_idle(Duration::from_secs(settings.time_to_idle_secs));
        }

        Self {
            cache: builder.build(),
        }
    }

    pub fn insert(&self, id: &str, bitmaps: Arc<CollectionBitmaps>) {
        self.cache.insert(id.to_string(), bitmaps);
    }

    pub fn get(&self, id: &str) -> Option<Arc<CollectionBitmaps>> {
        let key = id.to_string();
        self.cache.get(&key)
    }

    pub fn contains(&self, id: &str) -> bool {
        let key = id.to_string();
        self.cache.contains_key(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CollectionsSettings;

    fn test_settings(max_memory_bytes: u64) -> CollectionsSettings {
        CollectionsSettings {
            max_memory_bytes,
            time_to_live_secs: 0,
            time_to_idle_secs: 0,
            max_post_payload_bytes: 0,
        }
    }

    #[test]
    fn insert_and_get_round_trip() {
        let store = CollectionStore::new(&test_settings(1024 * 1024));
        let mut bmp = RoaringBitmap::new();
        bmp.insert(42);
        store.insert(
            "deck1",
            Arc::new(CollectionBitmaps {
                unique: bmp,
                nonunique: RoaringBitmap::new(),
            }),
        );
        let got = store.get("deck1").expect("collection present");
        assert!(got.unique.contains(42));
    }

    #[test]
    fn missing_returns_none() {
        let store = CollectionStore::new(&test_settings(1024 * 1024));
        assert!(store.get("missing").is_none());
    }
}
