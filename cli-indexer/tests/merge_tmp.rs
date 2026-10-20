use index_core::catalog::Catalog;
use index_core::keyword_catalog::KeywordCatalog;
use index_core::merge::{merge_indexes, MergeOptions};
use roaring::RoaringBitmap;
use serde_json::json;
use std::fs;
use std::path::Path;

fn write_json(path: &Path, value: serde_json::Value) {
    let text = serde_json::to_string_pretty(&value).expect("json");
    fs::write(path, text).expect("write json");
}

fn write_bitmap(path: &Path, bits: &[u32]) {
    let mut bmp = RoaringBitmap::new();
    for &b in bits {
        bmp.insert(b);
    }
    let mut bytes = Vec::new();
    bmp.serialize_into(&mut bytes).expect("serialize");
    fs::write(path, bytes).expect("write bitmap");
}

fn write_zero_cards(path: &Path, total_bit_span: u32) {
    let len = total_bit_span as usize * index_core::compact::RECORD_SIZE;
    fs::write(path, vec![0u8; len]).expect("write cards.bin");
}

fn make_set_index(root: &Path, set: &str, families: &[(&str, &str, u32)], idgd_bits: &[(u32, &[u32])]) {
    // families: (faction, family_number, max_unique_id) with implicit family_id = "{faction}_{family_number}"
    let set_dir = root.join(set);
    fs::create_dir_all(&set_dir).expect("mkdir set");

    let mut start_bit: u32 = 0;
    let mut families_out = Vec::new();
    let mut card_count: u32 = 0;
    for (faction, family_number, max_unique_id) in families {
        let family_id = format!("{faction}_{family_number}");
        families_out.push(json!({
          "start_bit": start_bit,
          "faction": *faction,
          "family_number": *family_number,
          "family_id": family_id,
          "max_unique_id": *max_unique_id,
          "card_count": *max_unique_id,
          "first_reference": format!("ALT_{set}_B_{faction}_{family_number}_U_1"),
          "name": {},
          "artist": "",
          "card_sub_types": [],
          "set": { "reference": set, "name": "", "code": null }
        }));
        start_bit += *max_unique_id;
        card_count += *max_unique_id;
    }

    let catalog = json!({
      "set": set,
      "faction_order": ["AX","BR","LY","MU","OR","YZ"],
      "families": families_out,
      "total_bit_span": start_bit
    });
    write_json(&set_dir.join("catalog.json"), catalog);

    let manifest = json!({
      "version": 1,
      "set": set,
      "root": format!("fixture:{set}"),
      "built_at_secs": 0,
      "card_count": card_count,
      "id_gd_count": 1,
      "total_bit_span": start_bit,
      "family_count": families.len()
    });
    write_json(&set_dir.join("manifest.json"), manifest);

    write_zero_cards(&set_dir.join("cards.bin"), start_bit);

    fs::create_dir_all(set_dir.join("id_gd")).expect("mkdir id_gd");
    for (id, bits) in idgd_bits {
        write_bitmap(&set_dir.join("id_gd").join(format!("{id}.roar")), bits);
    }
    // Add one per-line bitmap (m1) for the first idGd to ensure merge propagates it.
    if let Some((id, bits)) = idgd_bits.first() {
        write_bitmap(
            &set_dir.join("id_gd").join(format!("{id}_m1.roar")),
            bits,
        );
    }

    // Minimal idgd_catalog.json (merge only needs metadata; merge recomputes card_count/bytes).
    write_json(
        &set_dir.join("idgd_catalog.json"),
        json!({
          "set": set,
          "entries": idgd_bits.iter().map(|(id, _)| json!({
            "id_gd": *id,
            "card_count": 0,
            "bitmap_bytes": 0,
            "bitmap_file": format!("{id}.roar"),
            "element_type": "TRIGGER",
            "translations": {},
            "is_main": true,
            "is_echo": false,
            "m1": { "card_count": 0, "bitmap_bytes": 0, "bitmap_file": format!("{id}_m1.roar") }
          })).collect::<Vec<_>>()
        }),
    );
}

#[test]
fn merge_overlap_group_interleaves_families_and_preserves_set_for_decode() {
    let index_dir = tempfile::tempdir().expect("index tempdir");
    let merged_root = tempfile::tempdir().expect("merge out root");

    // COREKS and CORE overlap on AX_04; ALIZE is non-overlapping (BR_01 only).
    make_set_index(index_dir.path(), "COREKS", &[("AX", "04", 3)], &[(90, &[0])]);
    make_set_index(index_dir.path(), "CORE", &[("AX", "04", 3)], &[(90, &[0])]);
    make_set_index(index_dir.path(), "ALIZE", &[("BR", "01", 2)], &[(90, &[1])]);

    let out = merged_root.path().join("COREKS_CORE_ALIZE");
    let summary = merge_indexes(
        index_dir.path(),
        "COREKS,CORE,ALIZE",
        &out,
        MergeOptions::default(),
    )
    .expect("merge");
    assert_eq!(summary.total_bit_span, 3 + 3 + 2);

    // Overlap group (COREKS+CORE) interleaves within AX_04:
    // COREKS AX_04: bits 0..2, CORE AX_04: bits 3..5. ALIZE starts after that at 6..7.
    let merged_catalog = Catalog::load(&out.join("catalog.json")).expect("load merged catalog");
    let d0 = merged_catalog.decode_bit(0).expect("decode 0");
    assert_eq!(d0.reference, "ALT_COREKS_B_AX_04_U_1");
    let d3 = merged_catalog.decode_bit(3).expect("decode 3");
    assert_eq!(d3.reference, "ALT_CORE_B_AX_04_U_1");

    let br = merged_catalog
        .families
        .iter()
        .find(|f| f.family_id == "BR_01" && f.source_set.as_deref() == Some("ALIZE"))
        .expect("ALIZE BR_01 family");
    assert_eq!(br.start_bit, 6);

    // idGd=90 should contain COREKS bit 0, CORE bit 3, and ALIZE bit 7 (source bit 1 remapped to 6+1).
    let bmp = {
        let bytes = fs::read(out.join("id_gd/90.roar")).expect("read merged roar");
        RoaringBitmap::deserialize_from(&bytes[..]).expect("deserialize")
    };
    assert!(bmp.contains(0));
    assert!(bmp.contains(3));
    assert!(bmp.contains(7));
    assert_eq!(bmp.len(), 3);

    // Per-line bitmap should also be merged (we wrote 90_m1.roar for each source with same bits).
    let bmp_m1 = {
        let bytes = fs::read(out.join("id_gd/90_m1.roar")).expect("read merged roar m1");
        RoaringBitmap::deserialize_from(&bytes[..]).expect("deserialize m1")
    };
    assert!(bmp_m1.contains(0));
    assert!(bmp_m1.contains(3));
    assert!(bmp_m1.contains(7));
    assert_eq!(bmp_m1.len(), 3);
}

#[test]
fn merge_unions_keyword_catalogs_in_set_order() {
    let index_dir = tempfile::tempdir().expect("index tempdir");
    let merged_root = tempfile::tempdir().expect("merge out root");

    make_set_index(index_dir.path(), "COREKS", &[("AX", "04", 3)], &[(90, &[0])]);
    make_set_index(index_dir.path(), "ALIZE", &[("BR", "01", 2)], &[(90, &[1])]);
    write_json(
        &index_dir.path().join("COREKS/keywords.json"),
        json!({ "keywords": { "FLEETING": { "en_US": "Fleeting", "fr_FR": "Fugace" } } }),
    );
    write_json(
        &index_dir.path().join("ALIZE/keywords.json"),
        json!({ "keywords": {
            "FLEETING": { "en_US": "Ignored", "it_IT": "Fugace" },
            "RESUPPLY_LOW": { "fr_FR": "Ravitaillez" }
        } }),
    );

    let out = merged_root.path().join("ALL_SETS");
    merge_indexes(
        index_dir.path(),
        "COREKS,ALIZE",
        &out,
        MergeOptions::default(),
    )
    .expect("merge");

    let merged = KeywordCatalog::load_from_dir(&out).expect("merged keywords.json");
    assert_eq!(merged.name("FLEETING", "en_US"), Some("Fleeting"));
    assert_eq!(merged.name("FLEETING", "fr_FR"), Some("Fugace"));
    assert_eq!(merged.name("FLEETING", "it_IT"), Some("Fugace"));
    assert_eq!(merged.name("RESUPPLY_LOW", "fr_FR"), Some("Ravitaillez"));
}

