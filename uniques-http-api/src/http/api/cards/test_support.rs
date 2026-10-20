#![cfg(test)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use index_core::bitmap::EffectLine;
use index_core::catalog::{Catalog, FamilyCardSubType, FamilyEntry, FamilySet, FACTION_ORDER};
use index_core::compact::{encode_record, CompactCardFields, RECORD_SIZE};
use index_core::faction_index::Faction;
use index_core::idgd_catalog::{IdGdCatalog, IdGdCatalogEntry};
use index_core::idgd_collapse::IdGdAliasMap;
use index_core::keyword_catalog::KeywordCatalog;
use index_core::stat_index::StatField;
use roaring::RoaringBitmap;

use crate::http::state::AppState;
use crate::index::UniquesIndex;
use crate::index::loader::{
    build_family_lookup_index, build_family_span_groups, build_name_search_index,
    build_set_bitmaps, FactionsSummary, IndexManifest, StatsSummary, SET_CORE, SET_COREKS,
};

pub(crate) fn test_state() -> AppState {
    let catalog = Catalog {
        set: "TEST".to_string(),
        faction_order: FACTION_ORDER.iter().map(|s| s.to_string()).collect(),
        families: vec![FamilyEntry {
            start_bit: 0,
            faction: "AX".to_string(),
            family_number: "01".to_string(),
            family_id: "AX_01".to_string(),
            source_set: None,
            max_unique_id: 10,
            card_count: 10,
            first_reference: "ALT_TEST_B_AX_01_U_1".to_string(),
            name: BTreeMap::from([
                ("en_US".to_string(), "Test Card".to_string()),
                ("fr_FR".to_string(), "Élémentaire de Kélon".to_string()),
            ]),
            artist: "Test Artist".to_string(),
            card_sub_types: vec![FamilyCardSubType {
                reference: "ENGINEER".to_string(),
                name: BTreeMap::from([("en_US".to_string(), "Engineer".to_string())]),
            }],
            set: FamilySet {
                reference: "COREKS".to_string(),
                name: "Test Set".to_string(),
                code: Some("BTG".to_string()),
            },
        }],
        total_bit_span: 10,
    };

    let manifest = IndexManifest {
        version: 1,
        set: "TEST".to_string(),
        kind: None,
        built_at_secs: 0,
        card_count: 10,
        id_gd_count: 6,
        total_bit_span: 10,
        family_count: 1,
    };

    let idgd_catalog = IdGdCatalog {
        set: "TEST".to_string(),
        entries: vec![
            IdGdCatalogEntry {
                id_gd: 24,
                card_count: 2,
                bitmap_bytes: 0,
                bitmap_file: "24.roar".to_string(),
                element_type: "TRIGGER".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
                duplicated_id_gd: Vec::new(),
            },
            IdGdCatalogEntry {
                id_gd: 191,
                card_count: 1,
                bitmap_bytes: 0,
                bitmap_file: "191.roar".to_string(),
                element_type: "CONDITION".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
                duplicated_id_gd: Vec::new(),
            },
            IdGdCatalogEntry {
                id_gd: 42,
                card_count: 1,
                bitmap_bytes: 0,
                bitmap_file: "42.roar".to_string(),
                element_type: "OUTPUT".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: false,
                is_echo: true,
                duplicated_id_gd: Vec::new(),
            },
            IdGdCatalogEntry {
                id_gd: 90,
                card_count: 1,
                bitmap_bytes: 0,
                bitmap_file: "90.roar".to_string(),
                element_type: "OUTPUT".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
                duplicated_id_gd: Vec::new(),
            },
            IdGdCatalogEntry {
                id_gd: 25,
                card_count: 2,
                bitmap_bytes: 0,
                bitmap_file: "25.roar".to_string(),
                element_type: "TRIGGER".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
                duplicated_id_gd: Vec::new(),
            },
            IdGdCatalogEntry {
                id_gd: 192,
                card_count: 2,
                bitmap_bytes: 0,
                bitmap_file: "192.roar".to_string(),
                element_type: "CONDITION".to_string(),
                translations: BTreeMap::new(),
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
                duplicated_id_gd: Vec::new(),
            },
        ],
    };

    let stats_summary = StatsSummary {
        version: 1,
        set: "TEST".to_string(),
        total_cards_indexed: 10,
        fields: vec![],
    };
    let factions_summary = FactionsSummary {
        version: 1,
        set: "TEST".to_string(),
        total_cards_indexed: 10,
        source: "test".to_string(),
        factions: vec![],
        unknown_count: 0,
        bitmap_dir: "factions".to_string(),
    };

    let mut id_gd_per_line: BTreeMap<(u32, EffectLine), RoaringBitmap> = BTreeMap::new();
    id_gd_per_line.insert((24, EffectLine::M2), RoaringBitmap::from_iter([2, 5]));
    id_gd_per_line.insert((25, EffectLine::M1), RoaringBitmap::from_iter([7, 8]));
    id_gd_per_line.insert((25, EffectLine::M2), RoaringBitmap::from_iter([7]));
    id_gd_per_line.insert((191, EffectLine::M1), RoaringBitmap::from_iter([5]));
    id_gd_per_line.insert((192, EffectLine::M1), RoaringBitmap::from_iter([7, 8]));
    id_gd_per_line.insert((192, EffectLine::M2), RoaringBitmap::from_iter([7]));
    id_gd_per_line.insert((42, EffectLine::Ec), RoaringBitmap::from_iter([5]));
    id_gd_per_line.insert((90, EffectLine::M1), RoaringBitmap::from_iter([2]));

    let mut cards = vec![0u8; 10 * RECORD_SIZE];
    let ax_record = encode_record(&CompactCardFields {
        faction_code: 1,
        main_cost: 2,
        recall_cost: 3,
        mountain_power: 0,
        ocean_power: 0,
        forest_power: 0,
        main_effect: [[24, 191, 90], [0, 0, 0], [0, 0, 0]],
        echo_effect: [24, 0, 42],
    });
    for &idx in &[2u32, 5] {
        let off = idx as usize * RECORD_SIZE;
        cards[off..off + RECORD_SIZE].copy_from_slice(&ax_record);
    }

    let effects_list =
        crate::http::api::effects::build_effects_list(&idgd_catalog, &KeywordCatalog::default());
    let effects_body = Arc::new(
        crate::http::api::effects::serialize_effects_list(&effects_list).unwrap(),
    );
    let set_bitmaps = build_set_bitmaps(&catalog);
    let name_search_index = build_name_search_index(&catalog);
    let family_lookup_index = build_family_lookup_index(&catalog);
    let family_span_groups = build_family_span_groups(&catalog);

    let id_gd_aliases = IdGdAliasMap::from_catalog(&idgd_catalog);

    let index = UniquesIndex {
        index_dir: "C:\\tmp\\index".into(),
        catalog,
        manifest,
        idgd_catalog,
        id_gd_aliases,
        effects_body,
        stats_summary,
        factions_summary,
        cards,
        id_gd_whole: BTreeMap::new(),
        id_gd_per_line,
        stats: {
            let mut stats = BTreeMap::<StatField, [RoaringBitmap; 16]>::new();
            let mut main_cost: [RoaringBitmap; 16] =
                std::array::from_fn(|_| RoaringBitmap::new());
            main_cost[2] = RoaringBitmap::from_iter([2, 5]);
            stats.insert(StatField::MainCost, main_cost);

            let mut recall_cost: [RoaringBitmap; 16] =
                std::array::from_fn(|_| RoaringBitmap::new());
            recall_cost[3] = RoaringBitmap::from_iter([5]);
            stats.insert(StatField::RecallCost, recall_cost);

            let mut forest_power: [RoaringBitmap; 16] =
                std::array::from_fn(|_| RoaringBitmap::new());
            forest_power[1] = RoaringBitmap::from_iter([2]);
            forest_power[4] = RoaringBitmap::from_iter([5]);
            stats.insert(StatField::ForestPower, forest_power);

            let mut mountain_power: [RoaringBitmap; 16] =
                std::array::from_fn(|_| RoaringBitmap::new());
            mountain_power[2] = RoaringBitmap::from_iter([2, 5]);
            stats.insert(StatField::MountainPower, mountain_power);

            let mut ocean_power: [RoaringBitmap; 16] =
                std::array::from_fn(|_| RoaringBitmap::new());
            ocean_power[0] = RoaringBitmap::from_iter([2, 5]);
            stats.insert(StatField::OceanPower, ocean_power);

            stats
        },
        factions: {
            let mut factions = BTreeMap::<Faction, RoaringBitmap>::new();
            factions.insert(Faction::Ax, RoaringBitmap::from_iter([2, 5]));
            factions.insert(Faction::Br, RoaringBitmap::from_iter([7]));
            factions
        },
        set_bitmaps,
        name_search_index,
        family_lookup_index,
        family_span_groups,
        effects_body_gzip: None,
    };

    AppState::new_with_index(index)
}

fn family_entry(
    start_bit: u32,
    source_set: &str,
    family_number: &str,
    name: &str,
) -> FamilyEntry {
    FamilyEntry {
        start_bit,
        faction: "AX".to_string(),
        family_number: family_number.to_string(),
        family_id: format!("AX_{family_number}"),
        source_set: Some(source_set.to_string()),
        max_unique_id: 5,
        card_count: 5,
        first_reference: format!("ALT_{source_set}_B_AX_{family_number}_U_1"),
        name: BTreeMap::from([("en_US".to_string(), name.to_string())]),
        artist: "Test".to_string(),
        card_sub_types: vec![],
        set: FamilySet {
            reference: source_set.to_string(),
            name: source_set.to_string(),
            code: None,
        },
    }
}

pub(crate) fn test_state_with_sets() -> AppState {
    let catalog = Catalog {
        set: "ALL_SETS".to_string(),
        faction_order: FACTION_ORDER.iter().map(|s| s.to_string()).collect(),
        families: vec![
            family_entry(0, SET_CORE, "01", "Kelon Elemental"),
            family_entry(5, SET_COREKS, "01", "Kelon Elemental"),
            family_entry(10, "ALIZE", "02", "Other Character"),
        ],
        total_bit_span: 15,
    };

    let manifest = IndexManifest {
        version: 1,
        set: "ALL_SETS".to_string(),
        kind: Some("merge".to_string()),
        built_at_secs: 0,
        card_count: 15,
        id_gd_count: 0,
        total_bit_span: 15,
        family_count: 3,
    };

    let set_bitmaps = build_set_bitmaps(&catalog);
    let name_search_index = build_name_search_index(&catalog);
    let family_lookup_index = build_family_lookup_index(&catalog);
    let family_span_groups = build_family_span_groups(&catalog);
    assert!(set_bitmaps.by_set.contains_key(SET_CORE));
    assert!(set_bitmaps.by_set.contains_key(SET_COREKS));
    assert!(set_bitmaps.by_set.contains_key("ALIZE"));
    assert!(set_bitmaps.core_and_coreks.is_some());

    let index = UniquesIndex {
        index_dir: "C:\\tmp\\index".into(),
        catalog,
        manifest,
        idgd_catalog: IdGdCatalog {
            set: "ALL_SETS".to_string(),
            entries: vec![],
        },
        id_gd_aliases: IdGdAliasMap::default(),
        effects_body: Arc::new(Bytes::from_static(b"{}")),
        stats_summary: StatsSummary {
            version: 1,
            set: "ALL_SETS".to_string(),
            total_cards_indexed: 15,
            fields: vec![],
        },
        factions_summary: FactionsSummary {
            version: 1,
            set: "ALL_SETS".to_string(),
            total_cards_indexed: 15,
            source: "test".to_string(),
            factions: vec![],
            unknown_count: 0,
            bitmap_dir: "factions".to_string(),
        },
        cards: vec![0u8; 15 * RECORD_SIZE],
        id_gd_whole: BTreeMap::new(),
        id_gd_per_line: BTreeMap::new(),
        stats: BTreeMap::new(),
        factions: BTreeMap::new(),
        set_bitmaps,
        name_search_index,
        family_lookup_index,
        family_span_groups,
        effects_body_gzip: None,
    };

    AppState::new_with_index(index)
}
