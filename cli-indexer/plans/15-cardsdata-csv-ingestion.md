## Plan 15: CardsData CSV ingestion for unique prints (first slice)

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md) for the
cross-cutting decisions (D1-D7) this plan implements (D1, D2, D3, D5 specifically).

### Goal

Give `cli-indexer` a second way to build a unique-card index: reading directly from
[`CardsData`](https://github.com/Altered-Re-Union/CardsData)'s exported CSVs, instead of crawling
Equinox raw JSON (`index-core/src/crawl.rs`). Scope of this slice: **unique prints only**, feeding the
*existing, unchanged* `CatalogBuilder` / `compact_fields_from_card` pipeline — non-unique ingestion is
a separate lot (Lot 1).

### Why this shape

`CatalogBuilder::on_card` and `compact_fields_from_card` both take a `&CardJson` (see
[`index-core/src/card.rs`](../../index-core/src/card.rs)) — a plain Rust struct, not something tied to
parsing an actual JSON file. So instead of teaching those two (well-tested) functions a second input
shape, this slice adds an adapter that builds a `CardJson` **in memory** from joined CardsData CSV
rows, then hands it to the exact same functions the JSON-crawl path already uses. Per D2, nothing is
written to disk as an intermediate — the `CardJson` only exists as a value passed straight through.

### Inputs (CardsData CSVs, unique-only)

Confirmed real headers (see files under `CardsData/data/csv/`):

- `CardFamilies.csv`: `Id,CardTypeReference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT,FlavorText_*`
- `CardFamilySubTypes.csv`: `CardFamilyId,CardSubTypeReference` (one row per subtype, join to `CardFamilies.Id`)
- `CardSubTypes.csv`: `Reference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT` (localized subtype names)
- `Sets.csv`: `Reference,Name_en_US,...,IsBaseSet` (no short "code" — reuse existing `set_code::set_code()`)
- `Artists.csv`: `Id,NickName`
- `Unique/<SET>/UniquePrints.csv`: `Id,Reference,CardFamilyId,SetReference,FactionReference,ArtistId,InstanceNumber,CollectorNumberFormatted,SerializedNumber,IsPublic,IsSuspended,IsErrated,IsBanned,IsExclusive,IsOwnerless,IsSerialized,FOREST_POWER,MAIN_COST,MOUNTAIN_POWER,OCEAN_POWER,RECALL_COST` (fixed columns for uniques — no dynamic pivot, unlike `NonUniquePrints.csv`)
- `Unique/<SET>/UniquePrintEffectFragments.csv`: `UniquePrintId,DisplayIndex,TriggerId,ConditionId,EffectId`
- `EffectFragments.csv`: `IdGd,Type,KeywordReference,Slot,Text_en_US,Text_fr_FR,Text_es_ES,Text_de_DE,Text_it_IT`

Confirmed empirically:
- `Reference` on a `UniquePrints` row preserves the Equinox format verbatim
  (`ALT_<SET>_B_<faction>_<familyNumber>_U_<instance>`), even when that print's own
  `FactionReference` differs from the faction embedded in the reference (e.g.
  `ALT_CORE_B_AX_04_U_4744` with `FactionReference=MU` — a print whose randomized faction moved away
  from its family's nominal AX_04 slot). This means `index_core::path::parse_card_reference` can be
  reused as-is to get `ParsedCardPath` (nominal family location + instance number) directly from the
  `Reference` column — no new path-parsing code needed. `FactionReference` (not the parsed path) is
  what feeds `CardJson.main_faction`, exactly mirroring how the JSON-crawl path already separates
  "family location" (from the file path) from "this card's actual faction" (from JSON content).
- `EffectFragments.Slot` is populated **only on Trigger rows** (e.g. `24,Trigger,,MAIN_EFFECT,{J},...`);
  Condition/Output rows have an empty `Slot`. So resolving which slot (`MAIN_EFFECT` vs `ECHO_EFFECT`)
  a `UniquePrintEffectFragments` row belongs to means looking up `EffectFragments[row.TriggerId].Slot`
  — never the row's own `Slot`. This confirms D5: no CSV schema change needed, just this join.
- Two `CardFamilyId`s exist per card: CardsData's own numeric `CardFamilyId` (used only to look up
  family metadata) and the existing `{faction}_{familyNumber}` string `ParsedCardPath::family_id()`
  (used for bit-span grouping and reference reconstruction). They are **not the same key space** and
  must not be conflated — a family can (and does) contain prints whose own `FactionReference` differs
  from the family's nominal faction, which is fine because only the compact record's `faction_code`
  reads from `FactionReference`.

### Mapping to `CardJson`

| `CardJson` field | Source |
| --- | --- |
| `translations[locale].name` | `CardFamilies.Name_<locale>` (joined via `UniquePrints.CardFamilyId`) |
| `illustrator.nick_name` | `Artists.NickName` (joined via `UniquePrints.ArtistId`) — varies per print, not per family |
| `card_sub_types[]` | `CardFamilySubTypes` rows for this `CardFamilyId`, each resolved against `CardSubTypes.csv` for localized names |
| `card_set` | `Sets.csv` row for `UniquePrints.SetReference`; `code` via existing `set_code::set_code()` |
| `main_faction.reference` | `UniquePrints.FactionReference` (the print's own, not the family's nominal faction) |
| `card_elements[MAIN_COST\|RECALL_COST\|*_POWER]` | `UniquePrints.MAIN_COST` / `RECALL_COST` / `FOREST_POWER` / `MOUNTAIN_POWER` / `OCEAN_POWER` |
| `card_elements[MAIN_EFFECT].cardEffectDisplays[0..3]` | `UniquePrintEffectFragments` rows for this print with `Slot=MAIN_EFFECT` (via Trigger join), grouped and ordered by `DisplayIndex`, each producing one Trigger/Condition/Output `CardEffectElement` |
| `card_elements[ECHO_EFFECT].cardEffectDisplays[0]` | Same, filtered to `Slot=ECHO_EFFECT` |

Not mapped in this slice (out of scope, tracked as gaps, not silently dropped): `CollectorNumberFormatted`,
`SerializedNumber`, the `Is*` flags. None of these are read by `compact_fields_from_card` or
`family_metadata_from_card` today, so leaving them unmapped doesn't lose anything the existing
pipeline uses — but they'll matter once this pipeline is asked to carry more than what the unique
index already exposes.

### Implementation

New module: [`index-core/src/cardsdata.rs`](../../index-core/src/cardsdata.rs) (skeleton added in this
pass — see file). Depends on a new `csv` crate dependency in `index-core/Cargo.toml`.

- `CardsDataSet::load(root: &Path, set: &str) -> Result<CardsDataSet>` — reads the shared referentials
  once (`CardFamilies.csv`, `CardFamilySubTypes.csv`, `CardSubTypes.csv`, `Sets.csv`, `Artists.csv`,
  `EffectFragments.csv`) plus the per-set `UniquePrints.csv` / `UniquePrintEffectFragments.csv`, all
  indexed in `BTreeMap`s keyed by their id/reference columns.
- `CardsDataSet::unique_cards(&self) -> Result<Vec<(ParsedCardPath, CardJson)>>` — one entry per
  `UniquePrints` row, **sorted by `(faction_rank, family_number, unique_id)`** via
  `index_core::path::sort_key` before returning, since `CatalogBuilder::on_card` requires cards of the
  same family to arrive contiguously and in ascending `unique_id` order (today guaranteed by
  `crawl.rs`'s directory walk order; the CSV reader must guarantee it explicitly instead).

Not done in this pass (explicitly deferred, not silently skipped):
- Wiring a `--source cardsdata --cardsdata-root <dir>` option into `cli-indexer`'s `Build` command
  (`cli-indexer/src/cli.rs`) and `index_core::build::build` (today hardcoded to the JSON crawl path) —
  needs the dual-entry design noted in the decision log (B4) so both sources stay comparable during
  the switch-over.
- Tests. Once wired, port a couple of real CardsData print rows (e.g. `CardFamilyId=302`, the
  AX/MU-mixed family from `Unique/CORE/UniquePrints.csv`) as fixtures, mirroring
  `index-core/tests/card-json/*.json`, and assert the adapter's `CardJson` produces the same
  `FamilyMetadata` / `CompactCardFields` a hand-written equivalent JSON fixture would.

### Open item carried over

Artist name resolution (`Artists.NickName`) and set name/code resolution both assume every
`ArtistId` / `SetReference` referenced by a `UniquePrints` row exists in the corresponding referential
CSV. Not validated yet — a missing join key today panics (`.expect`) rather than failing a single row;
revisit once this is wired into a real build run over the full CardsData export.
