## Plan 16: standalone non-unique index (first slice)

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md) (D1-D9)
and [`15-cardsdata-csv-ingestion.md`](15-cardsdata-csv-ingestion.md) (the unique-side equivalent this
mirrors).

### Goal / scope cut

Lot 1 from the decision log: a non-unique index buildable and queryable **independently** of the
unique index (merging them is Lot 2). This first slice covers **faction, rarity, product,
serialization, and the 5 stats shared with uniques** (`MAIN_COST`/`RECALL_COST`/`*_POWER`).

`PERMANENT` / `RESERVE` are **out of scope permanently, not deferred**: per the user, they only mean
anything on Hero cards and no search over them is planned — so `NonUniquePrintRow` never reads those
two columns at all (not a TODO to pick back up later).

Still deferred (unlike `PERMANENT`/`RESERVE`, these genuinely are follow-up work):
- Family metadata (name/subtypes/artist) and effect search (text or id-based) — both need the
  `CardsDataSet` machinery from plan 15 and are bigger pieces on their own.
- Any HTTP endpoint — this slice stops at `cli-indexer`, buildable and inspectable, same as how plan
  15 shipped before its CLI wiring.

### Confirmed data shape

`NonUniquePrints.csv` has the **identical header on all 20 CardsData sets** (verified via grep, not
assumed): `Id,Reference,CardFamilyId,SetReference,FactionReference,RarityReference,ProductReference,
ArtistId,CollectorNumberFormatted,SerializedNumber,IsPublic,IsSuspended,IsErrated,IsBanned,
IsExclusive,IsOwnerless,IsSerialized,FOREST_POWER,MAIN_COST,MOUNTAIN_POWER,OCEAN_POWER,PERMANENT,
RECALL_COST,RESERVE`. Nothing is dynamically pivoted per set — a `CHARACTER` row just leaves
`PERMANENT`/`RESERVE` blank and a `LANDMARK_PERMANENT` row leaves the character stats blank. This is
simpler than plan 15's original assumption (carried over from initial exploration) that columns
varied per set.

`Rarities.csv`: `C`, `R`, `E`, `U` (only `C`/`R`/`E` expected on non-unique rows). `Products.csv`:
`B`, `A` (AltArt), `P` (Promotion) — this is where "alt-art" already lives (D4), no separate flag
needed.

Also present, same as unique (confirmed in plan 15 / D9): a `FOILER` placeholder row per set
(`ALT_<SET>_B_NE_FOILER_U` — note it's still `_U`-suffixed even under `NonUniquePrints.csv`). Filtered
by a plain `reference.contains("FOILER")` check — no regex path-parsing needed for non-unique at all
in this slice, since there's no per-family instance-number span the way uniques have; the raw
`Reference` string is just kept as an opaque stable id.

### Design

No `CardJson` detour this time (no family/effect data needed yet), and no `CatalogBuilder`-style
bit-span logic either (non-unique prints aren't numbered instances of a family the way uniques are —
each row is just one print, addressed by its own sequential `print_index`). New, self-contained
module: [`index-core/src/nonunique.rs`](../../index-core/src/nonunique.rs).

- `CompactNonUniqueFields` (9-byte record): `faction_code, rarity_code, product_code, is_serialized,
  main_cost, recall_cost, mountain_power, ocean_power, forest_power` — same encode/decode/
  `write_compact_records` shape as `compact.rs`'s unique records, sized for what this slice needs.
- `rarity_code_from_reference` / `product_code_from_reference`: hardcoded matches (`C`→1, `R`→2,
  `E`→3; `B`→1, `A`→2, `P`→3), same style as `compact::faction_code_from_reference` — no need to load
  `Rarities.csv`/`Products.csv` at all, the value spaces are small and stable.
- `NonUniqueCatalog`: just `{ set, references: Vec<String> }` — `print_index` is the position in this
  vec. No family spans in this slice.
- `NonUniqueIndexBuilder` / `NonUniqueIndex`: one bitmap per faction/rarity/product value, one
  `serialized.roar`, and the existing 16-bucket-per-value stat layout for the 5 shared stats — written
  under `<out>/<SET>/nonunique/{factions,rarity,product,stats}/...` + `nonunique/serialized.roar`.
  Deliberately not reusing `FactionIndexBuilder`/`StatIndexBuilder` from the unique pipeline: they're
  coupled to `CompactCardFields`, and forcing a shared abstraction for 2-3 categorical fields wasn't
  worth the indirection — see D10 in the decision log.
- `load_nonunique_prints(cardsdata_root, set) -> Result<Vec<NonUniquePrintRecord>>`: reads
  `NonUniquePrints.csv`, skips FOILER rows, sorts by `(card_family_id, reference)` (keeps a family's
  prints adjacent — no functional use yet, but free groundwork for Lot 5).

### CLI wiring

New subcommand `BuildNonunique` in `cli-indexer/src/cli.rs` (not folded into `Build`'s `--source`
flag): the output shape is different enough (no `families`/bit-span summary) that a separate
`BuildSummary`-equivalent reads better as its own command, consistent with how the CLI already has
one subcommand per distinct concern (`Build`, `Merge`, `AuditMissing`, ...).

### Verification

Same approach as plan 15: build against every real `NonUnique/<SET>/` in the CardsData checkout via
Docker, not just `cargo check` — a plausible-looking adapter over CSV data has already produced two
real surprises once (D9), so this isn't optional.
