## Plan 24: Collector number ingestion (uniques only)

### Goal

Ingest `collectorNumberFormatted` (e.g. `BTG-011-U-5`) so `uniques-http-api` can expose it and
search by it. Scoped to **uniques only** — non-unique collector numbers reset per edition/product
and use a different rarity-letter mapping (`R1`/`R2` → `R`/`F`), so they are **not** a family-invariant
value the way unique collector numbers are. Non-unique collector number is a separate future design,
not attempted here.

### Key finding

Sampling all 3 fixtures in `tests/card-json/` shows the format is
`{setCode}-{familyCollectorNumber}-U-{uniqueId}`:

| reference | collectorNumberFormatted | familyCollectorNumber |
|---|---|---|
| `ALT_COREKS_B_AX_06_U_5` | `BTG-011-U-5` | `011` |
| `ALT_COREKS_B_MU_22_U_3140` | `BTG-113-U-3140` | `113` |
| `ALT_COREKS_B_OR_16_U_6` | `BTG-134-U-6` | `134` |

`setCode` is already stored (`FamilySet.code`) and `uniqueId` is already arithmetic
(`Catalog::decode_bit`). Tested and ruled out: this is **not** derivable from `faction`/
`familyNumber`/`card_family_id` by any formula — `familyCollectorNumber` is genuinely independent
per-family data, but it **is** constant across every print in a family (confirmed pattern, not yet
cross-checked against two prints of the *same* real family — do that first against a real build).
So it slots into `FamilyMetadata`/`FamilyEntry` exactly like `card_family_id` did (no new per-print
storage, no change to `compact.rs`/`cards.bin`). The full string is never stored — reconstructed at
query time from `setCode` + `familyCollectorNumber` + `uniqueId`.

### Changes

#### `card.rs`

- Add `#[serde(default)] pub collector_number_formatted: Option<String>` to `CardJson` (top-level
  JSON key, already present in raw Equinox card JSON — no parsing change needed beyond adding the
  field).
- Add `fn family_collector_number(card: &CardJson) -> Option<String>`: split
  `collector_number_formatted` on `-`, return segment index 1 (e.g. `"011"` from `"BTG-011-U-5"`).
  Return `None` if the field is absent or doesn't have enough segments.
- Wire into `family_metadata_from_card`.

#### `catalog.rs`

- Add `collector_family_number: Option<String>` to `FamilyMetadata` and `FamilyEntry`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`, same pattern as `card_family_id` —
  old `catalog.json` files keep parsing).
- `CatalogBuilder::finalize_current`: copy `c.metadata.collector_family_number` into the new
  `FamilyEntry` field, next to `card_family_id`.

#### `cardsdata.rs` — deferred, not implemented in this pass

CardsData isn't vendored in this repo, so the exact CSV column (`CardFamilies.csv` own column vs.
a per-print field on `UniquePrints.csv`) can't be confirmed without a real checkout. Per project
convention (confirm via grep before writing ingestion code, as D11 did for `NonUniquePrints.csv`),
this is left unimplemented for now: `CardsDataSet::card_json_for_print` leaves
`collector_number_formatted: None`. The CardsData-sourced build path stays fully functional — the new
field is `Option`, so cards built via `--source cardsdata` simply won't expose `collectorNumber` yet.
Follow-up: grep the real CardsData checkout, wire the column, add `collector_family_number` the same
way JSON-crawl does.

### Verification

- Unit test in `card.rs` (or a new test near `family_metadata_from_card`'s existing test):
  `family_collector_number` on all 3 fixtures returns `011`/`113`/`134`.
- Docker, against a real build: pick one real family with 2+ prints, confirm both resolve to the
  same `collector_family_number` (the assumption this whole plan rests on, only checked against
  single-print-per-family fixtures so far).

### Out of scope

- Non-unique collector number (different numbering scheme, own future design).
- Wiring `cardsdata.rs` (deferred until the CSV shape is confirmed).
- Search/HTTP exposure — see `uniques-http-api/plans/26-collector-number-and-unified-search.md`.
