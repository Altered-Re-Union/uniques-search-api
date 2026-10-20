# `ALL_SETS` index format specification

This document specifies the on-disk format written by the `cli-indexer` tool for a **merged “all sets” index** (commonly named `ALL_SETS`), so an external project can load and query the index without depending on this repository.

The merged index is produced by:

```text
cli-indexer build --root "..\path\to\equinox-cards\cards-unique-CORE" --set CORE --out ./build/sets_index
cli-indexer build --root "..\path\to\equinox-cards\cards-unique-COREKS" --set COREKS --out ./build/sets_index
...
cli-indexer merge --index-dir ./build/sets_index --sets COREKS,CORE,... --out ./build/full_index/ALL_SETS
```

The **merged set name** is the final path component of `--out` (e.g. `ALL_SETS`). All files described below are written **directly under that folder**.

---

## Core concepts

### Card universe and `card_index`

All bitmap-based indexes in this folder operate over one shared global integer space:

- **`card_index`**: a zero-based `u32` used as the bit position in Roaring bitmaps.
- For the merged index, `card_index` spans **all cards from all source sets** (with set overlap handled as described in “Merge ordering” below).

The valid range is:

```text
0 <= card_index < catalog.total_bit_span
```

### Families, UniqueID alignment, and padding

Cards are grouped by **family**, defined by `family_id = "{faction}_{family_number}"` (e.g. `AX_04`).

Within a family, `card_index` is aligned with `UniqueID`:

```text
card_index = family.start_bit + (UniqueID - 1)
```

Each family occupies a contiguous span of size `max_unique_id`. If some UniqueIDs are missing on disk, their slot is considered **padding**:

- No bitmap should contain those bits.
- `cards.bin` rows for those bits are **all zero bytes**.

### Merge ordering (why decode needs `source_set`)

The merged index interleaves cards to keep shared families adjacent.

- Source indexes are provided in a precedence list `--sets SET_A,SET_B,...`.
- Sets are grouped into **overlap groups**: consecutive sets belong to the same group if they share at least one `family_id` with any earlier set in the group.
- For a single-set group, the entire set is appended as a contiguous block (fast copy).
- For a multi-set overlap group, ordering is:

```text
family_id (sorted by faction order, then family_number numeric)
  then source_set (in the overlap group’s set order)
    then UniqueID (1..=max_unique_id, padding preserved)
```

Because the same `family_id` can exist in multiple sets, the merged `catalog.json` includes an optional `source_set` per family entry, so decoding a `card_index` can produce the correct `ALT_<SET>_...` reference.

---

## Directory layout (merged index)

```text
ALL_SETS/
  catalog.json
  manifest.json
  cards.bin
  idgd_catalog.json
  keywords.json
  extra_catalog.json
  extra/
    <filter-id>.roar
  id_gd/
    <id_gd>.roar
    <id_gd>_m1.roar
    <id_gd>_m2.roar
    <id_gd>_m3.roar
    <id_gd>_ec.roar
  stats_summary.json
  stats/
    main_cost/
      00.roar .. 15.roar
    recall_cost/
    mountain_power/
    ocean_power/
    forest_power/
  factions_summary.json
  factions/
    AX.roar BR.roar LY.roar MU.roar OR.roar YZ.roar
```

All `.roar` files are **serialized Roaring bitmaps**.

---

## `catalog.json`

### Purpose

`catalog.json` defines the global `card_index` space and provides the mapping:

- `card_index` → `(family_id, UniqueID, reference)`

### JSON schema

```json
{
  "set": "ALL_SETS",
  "faction_order": ["AX", "BR", "LY", "MU", "OR", "YZ"],
  "families": [
    {
      "start_bit": 0,
      "faction": "AX",
      "family_number": "04",
      "family_id": "AX_04",
      "source_set": "COREKS",
      "max_unique_id": 5800,
      "card_count": 5798,
      "first_reference": "ALT_COREKS_B_AX_04_U_1"
    }
  ],
  "total_bit_span": 1234567
}
```

### Field definitions

- **`set`** (`string`): merged set name (folder name).
- **`faction_order`** (`string[]`): fixed order `["AX","BR","LY","MU","OR","YZ"]`.
- **`families`** (`FamilyEntry[]`): ordered by increasing `start_bit`.
- **`total_bit_span`** (`u32`): exclusive upper bound for valid `card_index` values.

`FamilyEntry`:

- **`start_bit`** (`u32`): first `card_index` in this family span.
- **`faction`** (`string`): two-letter faction code.
- **`family_number`** (`string`): two-digit family number (e.g. `"04"`).
- **`family_id`** (`string`): `"{faction}_{family_number}"`.
- **`source_set`** (`string`, optional): present for merged indexes; absent for single-set indexes.
- **`max_unique_id`** (`u32`): size of this family span in the bitspace.
- **`card_count`** (`u32`): number of actual card files indexed (≤ `max_unique_id` when gaps exist).
- **`first_reference`** (`string`): convenience string; should equal `ALT_<set>_B_<faction>_<family_number>_U_1`.

### Decoding algorithm

To decode a Reference ID from a `card_index`:

1. Find the last family entry where `start_bit <= card_index`.
2. Compute `unique_id = card_index - start_bit + 1`.
3. Error if `unique_id > max_unique_id` (padding region).
4. Determine set code:
   - `set_code = family.source_set` if present
   - otherwise `set_code = catalog.set`
5. Construct reference:

```text
ALT_{set_code}_B_{faction}_{family_number}_U_{unique_id}
```

### Rust example: load + decode

```rust
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Catalog {
    set: String,
    faction_order: Vec<String>,
    families: Vec<FamilyEntry>,
    total_bit_span: u32,
}

#[derive(Debug, Deserialize)]
struct FamilyEntry {
    start_bit: u32,
    faction: String,
    family_number: String,
    family_id: String,
    #[serde(default)]
    source_set: Option<String>,
    max_unique_id: u32,
    card_count: u32,
    first_reference: String,
}

fn load_catalog(path: &Path) -> anyhow::Result<Catalog> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn decode_reference(catalog: &Catalog, card_index: u32) -> anyhow::Result<String> {
    anyhow::ensure!(card_index < catalog.total_bit_span, "card_index out of range");

    let family = catalog
        .families
        .iter()
        .rfind(|f| f.start_bit <= card_index)
        .ok_or_else(|| anyhow::anyhow!("card_index below first family"))?;

    let unique_id = card_index - family.start_bit + 1;
    anyhow::ensure!(
        unique_id <= family.max_unique_id,
        "card_index in padding region for {}",
        family.family_id
    );

    let set_code = family.source_set.as_deref().unwrap_or(&catalog.set);
    Ok(format!(
        "ALT_{}_B_{}_{}_U_{}",
        set_code, family.faction, family.family_number, unique_id
    ))
}
```

---

## `cards.bin`

### Purpose

`cards.bin` is a fixed-width row store keyed by `card_index`. It stores small numeric stats and compact effect IDs (idGd) extracted from card JSON.

### File size

The file contains exactly `catalog.total_bit_span` records. Each record is 32 bytes:

```text
len(cards.bin) == catalog.total_bit_span * 32
```

### Record layout (32 bytes)

All integers are unsigned. `u16` fields are **little-endian**.

Header (6 bytes):

```text
offset  size  name            type   notes
0       1     faction_code    u8     0=unknown, 1..6 = AX..YZ
1       1     main_cost       u8     0..15
2       1     recall_cost     u8     0..15
3       1     mountain_power  u8     0..15
4       1     ocean_power     u8     0..15
5       1     forest_power    u8     0..15
```

Effect idGd slots (24 bytes, 12×`u16` LE):

```text
idx  meaning
0    MAIN_EFFECT group 0: TRIGGER
1    MAIN_EFFECT group 0: CONDITION
2    MAIN_EFFECT group 0: OUTPUT
3    MAIN_EFFECT group 1: TRIGGER
4    MAIN_EFFECT group 1: CONDITION
5    MAIN_EFFECT group 1: OUTPUT
6    MAIN_EFFECT group 2: TRIGGER
7    MAIN_EFFECT group 2: CONDITION
8    MAIN_EFFECT group 2: OUTPUT
9    ECHO_EFFECT: TRIGGER
10   ECHO_EFFECT: CONDITION
11   ECHO_EFFECT: OUTPUT
```

Offsets:

```text
offset  size  field
6       2     id_gd[0]  (u16 LE)
8       2     id_gd[1]
...
28      2     id_gd[11]
30      2     reserved (currently always 0)
```

### All-zero rows

Rows corresponding to padding slots (missing UniqueIDs) are **all 0 bytes**. A consumer can treat such rows as “no card present”.

### Rust example: mmap and read rows

```rust
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

const RECORD_SIZE: usize = 32;

struct CardRow<'a> {
    buf: &'a [u8; RECORD_SIZE],
}

impl<'a> CardRow<'a> {
    fn from_slice(data: &'a [u8], card_index: u32) -> Option<Self> {
        let off = card_index as usize * RECORD_SIZE;
        let slice = data.get(off..off + RECORD_SIZE)?;
        let buf: &[u8; RECORD_SIZE] = slice.try_into().ok()?;
        Some(Self { buf })
    }

    fn faction_code(&self) -> u8 { self.buf[0] }
    fn main_cost(&self) -> u8 { self.buf[1] }
    fn recall_cost(&self) -> u8 { self.buf[2] }
    fn mountain_power(&self) -> u8 { self.buf[3] }
    fn ocean_power(&self) -> u8 { self.buf[4] }
    fn forest_power(&self) -> u8 { self.buf[5] }

    fn id_gd(&self, idx: usize) -> u16 {
        let base = 6 + idx * 2;
        u16::from_le_bytes([self.buf[base], self.buf[base + 1]])
    }

    fn is_all_zero(&self) -> bool {
        self.buf.iter().all(|&b| b == 0)
    }
}

fn open_cards_bin(path: &Path) -> anyhow::Result<Mmap> {
    let f = File::open(path)?;
    Ok(unsafe { Mmap::map(&f)? })
}
```

---

## `id_gd/` bitmaps (`*.roar`)

### Purpose

For each `idGd` value present in the merged index, the indexer writes one or more Roaring bitmaps under `id_gd/`:

| File | Meaning |
|------|---------|
| `{id_gd}.roar` | **Whole-card** index: set bit if the card contains this `idGd` anywhere in its effect text (any line, main or echo). |
| `{id_gd}_m1.roar` … `{id_gd}_m3.roar` | **Per-line** index for `MAIN_EFFECT` groups 1..3: set bit only if `idGd` appears on that specific effect line. |
| `{id_gd}_ec.roar` | **Per-line** index for `ECHO_EFFECT`: set bit only if `idGd` appears on the echo line. |

`cli-indexer query` uses per-line files by default (same-line matching). Pass `--whole-card` to use only `{id_gd}.roar`.

### File naming

- **Whole-card path**: `id_gd/{id_gd}.roar`
- **Per-line paths**: `id_gd/{id_gd}_m1.roar`, `{id_gd}_m2.roar`, `{id_gd}_m3.roar`, `{id_gd}_ec.roar`
- **`id_gd` type**: decimal `u32` in the filename

Per-line files are omitted when empty (not written to disk).

### Serialization format

Files contain `RoaringBitmap` serialized using the Rust `roaring` crate’s binary format (`serialize_into` / `deserialize_from`).

The easiest way for a Rust consumer to read it is to use the same crate: [roaring-rs (GitHub)](https://github.com/RoaringBitmap/roaring-rs)

### Rust example: load bitmap and iterate card indexes

```rust
use roaring::RoaringBitmap;
use std::fs;
use std::path::Path;

fn load_roar(path: &Path) -> anyhow::Result<RoaringBitmap> {
    let bytes = fs::read(path)?;
    Ok(RoaringBitmap::deserialize_from(&bytes[..])?)
}

fn example_iter(index_root: &Path, id_gd: u32) -> anyhow::Result<()> {
    let bmp = load_roar(&index_root.join("id_gd").join(format!("{id_gd}.roar")))?;
    for card_index in bmp.iter().take(10) {
        println!("card_index={card_index}");
    }
    Ok(())
}
```

---

## `idgd_catalog.json`

### Purpose

`idgd_catalog.json` is an inventory of all `idGd` values present in the merged index, including:

- the whole-card bitmap filename and size,
- optional per-line bitmap metadata (`m1`, `m2`, `m3`, `ec`),
- whether the idGd is main-effect or echo-effect scoped (`is_echo`),
- `element_type` and translated text for query/display.

### JSON schema

```json
{
  "set": "ALL_SETS",
  "entries": [
    {
      "id_gd": 191,
      "card_count": 12345,
      "bitmap_bytes": 9876,
      "bitmap_file": "191.roar",
      "element_type": "CONDITION",
      "is_echo": false,
      "translations": {
        "en_US": { "locale": "en_US", "text": "..." },
        "fr_FR": { "locale": "fr_FR", "text": "..." }
      },
      "m1": {
        "card_count": 8000,
        "bitmap_bytes": 4500,
        "bitmap_file": "191_m1.roar"
      },
      "m2": {
        "card_count": 1200,
        "bitmap_bytes": 900,
        "bitmap_file": "191_m2.roar"
      }
    }
  ]
}
```

Nested keys `m1`, `m2`, `m3`, `ec` are omitted when the corresponding per-line bitmap was not written.

### Field definitions

- **`set`** (`string`): merged set name.
- **`entries`** (`IdGdCatalogEntry[]`)

`IdGdCatalogEntry`:

- **`id_gd`** (`u32`): the idGd integer value.
- **`card_count`** (`u64`): cardinality of the **whole-card** bitmap `{id_gd}.roar`.
- **`bitmap_bytes`** (`u64`): byte length of `id_gd/<id_gd>.roar`.
- **`bitmap_file`** (`string`): filename only (e.g. `"191.roar"`); relative to `id_gd/`.
- **`element_type`** (`string`): one of `"TRIGGER"`, `"CONDITION"`, `"OUTPUT"` (used by `cli-indexer query` for bucket grouping).
- **`is_echo`** (`boolean` or `null`):
  - `false` — idGd appears only under **MAIN_EFFECT** lines (`m1`..`m3`).
  - `true` — idGd appears only under **ECHO_EFFECT** (`ec`).
  - `null` — idGd was indexed in **both** regions (data error; build logs an error).
- **`translations`** (`object`): map of locale key → `{ "locale": string, "text": string }`.
- **`duplicated_id_gd`** (`u32[]`, optional): non-canonical idGd values collapsed into this entry during index build/merge (same `element_type` and whitespace-normalized `en_US` text). Omitted when empty. Queries accept these ids and resolve them to the canonical `id_gd`. When duplicates differ only by whitespace, the canonical entry prefers the NBSP-bearing raw text.
- **`m1`**, **`m2`**, **`m3`**, **`ec`** (`object`, optional): per-line bitmap metadata with:
  - **`card_count`** (`u64`): cardinality of that line’s bitmap.
  - **`bitmap_bytes`** (`u64`): byte length of the file.
  - **`bitmap_file`** (`string`): e.g. `"191_m1.roar"` (relative to `id_gd/`).

### Rust example: load the catalog

```rust
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct IdGdCatalog {
    set: String,
    entries: Vec<IdGdCatalogEntry>,
}

#[derive(Debug, Deserialize)]
struct BitmapMeta {
    card_count: u64,
    bitmap_bytes: u64,
    bitmap_file: String,
}

#[derive(Debug, Deserialize)]
struct IdGdCatalogEntry {
    id_gd: u32,
    card_count: u64,
    bitmap_bytes: u64,
    bitmap_file: String,
    element_type: String,
    is_echo: Option<bool>,
    translations: BTreeMap<String, LocaleText>,
    m1: Option<BitmapMeta>,
    m2: Option<BitmapMeta>,
    m3: Option<BitmapMeta>,
    ec: Option<BitmapMeta>,
}

#[derive(Debug, Deserialize)]
struct LocaleText {
    locale: String,
    text: String,
}

fn load_idgd_catalog(path: &Path) -> anyhow::Result<IdGdCatalog> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}
```

---

## `keywords.json`

### Purpose

Printed names of the keywords that effect texts reference as `[CODE]` (e.g. `[FLEETING]`), per
locale. The HTTP API uses it to print keywords in bold in the `formattedText` of
`GET /api/v2/effects`.

### Source

`cli-indexer build` reads them from the card JSON, on each effect element:
`cardEffectElementDisplays[].cardKeyword.reference` is the code, and
`cardKeyword.translations[<locale>].displayWeb` is the printed name. Keywords with an empty
`reference` (the `[]` empty part) are skipped. `cli-indexer merge` writes the union of the source
files; for the same code and locale, the first set in `--sets` order wins.

The file is optional: indexes built before it existed have none, and the API then leaves
`[CODE]` as written in the formatted texts. When it loads an index, the API logs the codes used
in effect texts that have no name in `keywords.json` (`warning: keyword codes without a printed
name ...`) and those printed in English for some locales, so a rebuilt index can be checked from
the startup log.

Names are taken as Equinox wrote them, e.g. `RESUPPLY_LOW` → `Ravitaillez` / `Resupply` /
`reabastece` (lowercase in Spanish), and `SCOUT_1` → `Scout` without the number (cards print the
cost as the `{1}` that follows: `[SCOUT_1] {1}`).

### JSON schema

```json
{
  "keywords": {
    "FLEETING": {
      "de_DE": "Vergänglich",
      "en_US": "Fleeting",
      "es_ES": "Fugacidad",
      "fr_FR": "Fugace",
      "it_IT": "Fugace"
    }
  }
}
```

---

## `stats/` and `stats_summary.json`

### Purpose

The `stats/` directory contains “column indexes” for numeric stat values stored in `cards.bin`.

- Each stat field has up to 16 bucket bitmaps (`00`..`15`).
- Bucket bitmap `stats/<field>/<value>.roar` contains all `card_index` where that stat equals `value`.

### Bucket file naming

- `stats/main_cost/07.roar` means `main_cost == 7`
- Value filenames are always **two digits**, `00`..`15`.

### `stats_summary.json` schema

```json
{
  "version": 1,
  "set": "ALL_SETS",
  "total_cards_indexed": 123456,
  "fields": [
    {
      "field": "main_cost",
      "element_reference": "MAIN_COST",
      "counts": { "0": 100, "7": 2000 },
      "bitmap_dir": "stats/main_cost"
    }
  ]
}
```

Notes:

- `counts` keys are JSON numbers when parsed by serde into `BTreeMap<u8, u64>`, but may appear as strings depending on parser; treat them as the integer value 0..15.
- Only non-zero buckets are included in `counts` in practice, but consumers should handle missing entries as zero.

Fields present:

- `main_cost` (`MAIN_COST`)
- `recall_cost` (`RECALL_COST`)
- `mountain_power` (`MOUNTAIN_POWER`)
- `ocean_power` (`OCEAN_POWER`)
- `forest_power` (`FOREST_POWER`)

---

## `factions/` and `factions_summary.json`

### Purpose

The `factions/` directory contains one bitmap per `mainFaction.reference` (derived from JSON, stored as `faction_code` in `cards.bin`):

- `factions/AX.roar` contains all `card_index` where `mainFaction.reference == "AX"`, etc.

Cards with unknown or missing `mainFaction.reference` appear in no faction bitmap and are counted in `unknown_count`.

### `factions_summary.json` schema

```json
{
  "version": 1,
  "set": "ALL_SETS",
  "total_cards_indexed": 123456,
  "source": "mainFaction.reference",
  "factions": [
    { "reference": "AX", "card_count": 1000, "bitmap_file": "factions/AX.roar" }
  ],
  "unknown_count": 42,
  "bitmap_dir": "factions"
}
```

---

## `extra_catalog.json`

Optional registry of user-defined card-list filters (see `cli-indexer add-extra-filter`).

```json
{
  "version": 1,
  "set": "ALL_SETS",
  "entries": [
    {
      "id": "exclude-banned",
      "type": "property",
      "negated": true,
      "card_count": 1200,
      "bitmap_bytes": 4500,
      "bitmap_file": "extra/exclude-banned.roar"
    }
  ]
}
```

- **`id`**: stable filter slug; bitmap at `extra/<id>.roar`.
- **`type`**: optional `"format"` or `"property"` for downstream grouping.
- **`negated`**: when `true`, the bitmap stores an **exception list** (combine with AND NOT at query time); when `false`, an **include list** (AND).
- **`card_count` / `bitmap_bytes`**: Roaring cardinality and serialized file size at registration time.

---

## `manifest.json` (merged index)

### Purpose

`manifest.json` is a high-level summary and provenance record for the merged index build.

### JSON schema

```json
{
  "version": 1,
  "set": "ALL_SETS",
  "kind": "merge",
  "built_at_secs": 1710000000,
  "card_count": 123456,
  "id_gd_count": 7890,
  "total_bit_span": 130000,
  "family_count": 999,
  "merge": {
    "index_dir": "C:\\path\\to\\per_set_indexes",
    "source_sets": ["COREKS", "CORE", "ALIZE"],
    "source_manifests": [
      { "set": "COREKS", "card_count": 40000, "total_bit_span": 45000 }
    ]
  }
}
```

### Field definitions

- **`version`** (`u32`): currently `1`.
- **`set`** (`string`): merged set name (folder name).
- **`kind`** (`string`): `"merge"` for merged indexes.
- **`built_at_secs`** (`u64`): unix epoch seconds when written.
- **`card_count`** (`u32`): sum of `card_count` from all source manifests.
- **`id_gd_count`** (`usize` in writer; parse as `u64` safely): number of idGd bitmap files actually written (non-empty after merge).
- **`total_bit_span`** (`u32`): equals `catalog.total_bit_span`.
- **`family_count`** (`usize` in writer; parse as `u64` safely): number of family entries in `catalog.json`.
- **`merge`**: provenance:
  - `index_dir`: the parent folder containing the per-set indexes that were merged
  - `source_sets`: the ordered set list used for merge planning
  - `source_manifests`: per-set `{ set, card_count, total_bit_span }` from each input manifest

---

## Practical extraction recipes

### “Give me all card references containing idGd N”

1. Load `catalog.json`.
2. Load `id_gd/N.roar` (whole-card bitmap) and iterate its `card_index` values.
3. For each `card_index`, decode reference using the catalog.

Or use `cli-indexer query --id-gd N` (per-line by default; add `--whole-card` for step 2 only).

### “Intersect multiple idGd constraints” (e.g. trigger 24 AND condition 191)

**Default (`cli-indexer query`, per-line index)** — ids must match on the **same effect line**:

1. Group requested ids by `element_type` from `idgd_catalog.json` (TRIGGER / CONDITION / OUTPUT).
2. For each line `m1`, `m2`, `m3`, `ec`:
   - Union per-line bitmaps within each non-empty bucket (e.g. `24_m1.roar` for triggers).
   - Intersect buckets on that line.
3. Union results across lines.

Example: `--id-gd 24,191` matches cards where trigger 24 and condition 191 appear together on one line. It does **not** match a card with trigger 24 on line 1 and condition 191 on line 2.

**Legacy (`--whole-card`)** — ids may appear on different lines on the same card:

1. Union all trigger ids using `{id}.roar` files.
2. Union all condition ids using `{id}.roar` files.
3. Intersect the bucket groups (same as pre–per-line `cli-indexer` behavior).

Roaring bitmaps support fast set ops for custom consumers:

```rust
use roaring::RoaringBitmap;

fn intersect_all(mut it: impl Iterator<Item = RoaringBitmap>) -> RoaringBitmap {
    let mut acc = match it.next() {
        Some(b) => b,
        None => return RoaringBitmap::new(),
    };
    for b in it {
        acc &= b;
    }
    acc
}
```

### “Read stats/effects for the first K matches”

For each matching `card_index`:

- Decode reference with `catalog.json`.
- Read the row at `card_index` from `cards.bin` and extract numeric stats and idGd slots.

---

## Compatibility notes and constraints

- **Endianness**: all `u16` values in `cards.bin` are little-endian.
- **Types**:
  - `card_index`, `start_bit`, `total_bit_span`, `max_unique_id` are `u32`.
  - Bitmap cardinalities are `u64`.
- **Sparse output**:
  - `id_gd/<id>.roar` exists only if the whole-card bitmap is non-empty.
  - `id_gd/<id>_m1.roar` (and `_m2`, `_m3`, `_ec`) exist only if that per-line bitmap is non-empty.
  - `stats/<field>/<value>.roar` exists only for non-empty buckets.
  - `factions/<FACTION>.roar` exists only if that faction occurs at least once.
- **`idgd_catalog.json`**:
  - `element_type` is authoritative for query bucketing into TRIGGER/CONDITION/OUTPUT.
  - `is_echo` distinguishes main-effect vs echo-effect idGds (`null` = indexed in both; treat as invalid).
  - Nested `m1`/`m2`/`m3`/`ec` objects are omitted when the corresponding per-line file was not written.
- **Query tools**: `cli-indexer query` and `bench-query` default to per-line bitmaps; `--whole-card` selects `{id}.roar` only. Indexes built before per-line bitmaps were added must be rebuilt for default query to return results.

---

## Reference implementation sources

This format is defined by the `index-core` library and `cli-indexer` CLI:

- `index-core/src/merge.rs` (merged index writer)
- `index-core/src/catalog.rs` (catalog schema + decode)
- `index-core/src/compact.rs` (`cards.bin` record layout)
- `index-core/src/idgd_catalog.rs` (`idgd_catalog.json` schema, including `is_echo`)
- `index-core/src/query.rs` (per-line vs `--whole-card` query)
- `cli-indexer/src/bench_query.rs` (benchmark preload + query modes)
- `index-core/src/stat_index.rs` (`stats_summary.json` + bucket layout)
- `index-core/src/faction_index.rs` (`factions_summary.json` + bitmap layout)

