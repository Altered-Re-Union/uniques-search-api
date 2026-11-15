# Multi-rarity search refonte — decision log

**Status:** living document, updated as decisions are made, before any lot below is implemented.

This tracks the cross-cutting architecture decisions behind extending the search API from
uniques-only to all card rarities, sourced from the new [`CardsData`](https://github.com/Altered-Re-Union/CardsData)
referential repo. It complements the per-crate `plans/NN-*.md` files (see
[`cli-indexer/plans/`](../cli-indexer/plans/), [`uniques-http-api/plans/`](../uniques-http-api/plans/)):
those describe *how* a slice is implemented once scoped; this document records *why* the batches are
shaped the way they are.

## Goals

1. Search non-unique cards, not just uniques, without bolting a pile of special cases onto the
   existing unique-only query logic.
2. Search on abilities that occur more than once on a unique (already partially supported via
   `effect[N][matchCount]`, see [`14-effect-match-count-filtering.md`](../uniques-http-api/plans/14-effect-match-count-filtering.md)).
3. Page-number pagination (jump directly to page 12), instead of the current raw `card_index`
   cursor — both for multi-session browsing of a whole unique category, and to cap per-request
   payload size for the card renderer.
4. Let a "card family" (name/subtypes stored once per group of prints) emerge as a first-class API
   concept, shared across unique and non-unique prints of the same character.

## Batches

| Lot | Scope | Status |
| --- | --- | --- |
| 0 | CardsData → `cli-indexer` ingestion foundations (CSV reader, canonical family, effect slot resolution) | **done for unique prints** — [`cli-indexer/plans/15-cardsdata-csv-ingestion.md`](../cli-indexer/plans/15-cardsdata-csv-ingestion.md), [`index-core/src/cardsdata.rs`](../index-core/src/cardsdata.rs), wired as `cli-indexer build --source cardsdata`; all 7 CardsData unique sets build cleanly end-to-end (see D9) |
| 1 | Standalone non-unique index (base filters, no effect search yet) | **first slice done** — [`cli-indexer/plans/16-nonunique-index.md`](../cli-indexer/plans/16-nonunique-index.md), [`index-core/src/nonunique.rs`](../index-core/src/nonunique.rs), `cli-indexer build-nonunique`; all 20 CardsData non-unique sets build cleanly (see D11). Family metadata and effect search still deferred; `PERMANENT`/`RESERVE` stats are out of scope permanently (D12), not a TODO |
| 2 | Merge unique + non-unique search, concatenated non-unique-first | **done except Lot 4** — [`uniques-http-api/plans/19-merged-search.md`](../uniques-http-api/plans/19-merged-search.md)/[`21`](../uniques-http-api/plans/21-search-page-pagination.md), `GET /api/v2/search` (see D13, D15, D16, D18, D20): faction/rarity/product/serialized/edition/type/subtype/family/banned/errated/suspended + 5 cost/power predicates, cross-index `page`, `format` (unique-branch only), `collection` (both branches, D20), full `CardV2` parity for the unique side. Only effect filters (Lot 4) remain |
| 3 | Page-number pagination via bitmap `rank`/`select`, incl. `withFamilies` | **done for `/api/v2/cards`** — [`uniques-http-api/plans/20-page-pagination.md`](../uniques-http-api/plans/20-page-pagination.md), validated end-to-end (see D14). `/api/v2/search` (Lot 2) doesn't have `page` yet — cross-index paging needs its own follow-up |
| 4 | Multi-occurrence effect search generalized to non-unique (gated on CardsData's decomposition work) | **interim shipped** — [`uniques-http-api/plans/24-interim-effect-search.md`](../uniques-http-api/plans/24-interim-effect-search.md) (D19): `effectText` (non-unique, text) + `effect[N]`/`matchCount` (unique, ids) on `/api/v2/search`. The real fix (non-unique on shared ids) is still gated on CardsData's decomposition work |
| 5 | Family as a first-class API concept across both rarities | **done** — [`cli-indexer/plans/23-family-catalog.md`](../cli-indexer/plans/23-family-catalog.md), `GET /api/v2/family/{id}` plus a `family=` filter on `/api/v2/search` (see D17, D18). Non-unique multi-set builds now exist (D18), so the family catalog can span more than one set too |

## Decisions

### D1 — CardsData replaces Equinox as `cli-indexer`'s ingestion source

CardsData's whole purpose is to be the corrected, consolidated referential that Equinox's raw JSON
(scattered across many repos, with known data-quality issues) is not. `cli-indexer` should stop
cloning/parsing raw Equinox JSON and read from CardsData instead.

### D2 — No intermediate per-card JSON is reconstructed, anywhere

`cli-indexer` gains a new CSV reader that goes directly from CardsData's CSVs (prints, effect
fragments, referentials) to the compact binary index records — skipping any "rebuild a JSON file per
card" step. This avoids the GitHub repo-size ceiling CardsData would otherwise hit if it tried to
materialize per-card JSON, and it's not a performance concern either: this transform runs at index
**build time** (on CardsData push / manual publish), never per HTTP request, and CSV parsing into
flat records is cheaper than the current JSON parsing.

### D3 — CardsData's `CardFamilyId` becomes the canonical family key

`cli-indexer`'s current family matching (grouping consecutive cards by `(name, cardType)` at build
time, in [`index-core/src/catalog.rs`](../index-core/src/catalog.rs)) is replaced by consuming
CardsData's already-built `CardFamilies.csv` / `CardFamilySubTypes.csv` directly. Removes a source of
matching bugs and is the natural hook for sharing family metadata between unique and non-unique
prints (Lot 5).

### D4 — Rarity/product/alt-art/serialized are per-print fields, not a new family-like layer

Unlike name/type/subtypes (invariant across every print of a family — the reason they live at the
family level), rarity/product/alt-art/serialized vary *between* prints of the same family by
definition (that's why a non-unique family has several rows). They're modeled as small per-print
fields on the non-unique compact record (a couple of extra bytes, ~32 → ~34-35 bytes) plus one small
bitmap per value, mirroring the existing faction/set bitmap pattern — not a new normalization layer.
Card type stays family-level metadata (D3), since it genuinely doesn't vary within a family.

### D5 — Effect slot (main/echo) needs no CSV schema change

Initially assumed lost in the `PrintEffectFragments` pivot (two rows sharing the same `DisplayIndex`
for different slots looked ambiguous). Corrected: slot is a property of the **Trigger** fragment,
via the `Slot` column on `EffectFragments.csv`, resolved by following each row's `TriggerId`. The
`cli-indexer` CSV reader must do that join to bucket a row into M1/M2/M3 vs. Echo — see open question
Q1 below. Extending `Slot` into a first-class search dimension (beyond its current internal use for
per-line bitmaps) is a candidate for Lot 4, not decided yet.

### D6 — Non-unique effect search starts as text search, not id-based

CardsData's effect decomposition into shared Trigger/Condition/Output templates (positive `IdGd`) is
finished for uniques but not for non-uniques — deliberately deferred, a large task on its own.
Plugging non-unique effects into the same id-based `effect[N][t|c|o]` mechanism today would expose a
near-one-id-per-print space (freeform negative ids, deduplicated only by exact text so far), which is
not a meaningful search dimension yet.

Instead: non-unique effect search ships as a text substring filter over effect text, reusing the
existing `name` filter's mechanism (case/accent-insensitive substring, see
[`09-search-by-name.md`](../uniques-http-api/plans/09-search-by-name.md)) — not a new one. Once
CardsData finishes non-unique decomposition + cross-card dedup, non-unique effect search migrates to
the same id-based system as uniques; the text search stays alongside it (same relationship `name`
substring search has to exact-reference lookup today), nothing gets thrown away.

### D7 — No id-count limit exists today; keep GET for now, revisit if needed

Verified in [`parse_id_list`](../uniques-http-api/src/http/api/cards/parse.rs#L201-L219): the
comma-separated id list for `effect[N][t|c|o]` has no application-level size cap. This means a future
"resolve text → ids, then filter by those ids" flow isn't blocked by the app itself. The only
practical ceiling is GET query-string length (server/proxy dependent, commonly ~8 KB). If that flow
is ever built against a large freeform id space, prefer a POST-based variant (precedent:
`POST /api/v2/collection/{id}` already uses `DefaultBodyLimit`, see
[`collections.rs`](../uniques-http-api/src/http/api/collections.rs)) over stretching the GET query
string.

### Context note — the RAM/crash concern already got a partial client-side fix

[`demo-ui/plans/06-lazy-loading-card-renderer.md`](../demo-ui/plans/06-lazy-loading-card-renderer.md)
already addresses part of this: `withFamilies` can return ~400 families in one unpaginated response,
and the renderer now lazy-mounts/unmounts canvases (concurrency cap 4) to avoid mounting all of them
at once. That mitigates the rendering cost but not the root cause — the whole matching result set is
still fetched and held as JSON in one response. Lot 3 (page-number pagination applied to
`withFamilies` too) addresses the root cause.

### D8 — CSV rows adapt into the existing `CardJson`, no changes to `card.rs`/`catalog.rs`/`compact.rs`

Confirmed while writing the [B1 skeleton](../index-core/src/cardsdata.rs): `CatalogBuilder::on_card`
and `compact_fields_from_card` both take a plain `&CardJson` value, not something tied to parsing an
actual file — so the CSV reader builds a `CardJson` in memory from joined CardsData rows and hands it
to those two functions completely unchanged. Zero regression risk to the tested unique pipeline for
this increment. Confirmed empirically along the way:
- A `UniquePrints.Reference` value preserves the Equinox reference format verbatim
  (`ALT_<SET>_B_<faction>_<familyNumber>_U_<instance>`) even when that row's own `FactionReference`
  differs from the faction embedded in the reference — so `index_core::path::parse_card_reference`
  is reused as-is for `ParsedCardPath`, no new path-parsing code needed.
- `EffectFragments.Slot` is populated only on `Type=Trigger` rows (confirmed: id 24 →
  `Trigger, Slot=MAIN_EFFECT`); Condition/Output rows have an empty `Slot`. Resolving a
  `PrintEffectFragments` row's slot means following its `TriggerId`, never reading the row's own
  `Slot` column (which doesn't exist on that table anyway).

**Verified via Docker.** No Rust toolchain was on `PATH` in the session that wrote this skeleton, so it
was first reviewed by hand, then actually built using the `rust:1.86-bookworm` image already used by
the repo's own [`Dockerfile`](../Dockerfile) (`docker run -v "$(pwd):/app" -w /app rust:1.86-bookworm
cargo check -p index-core` / `cargo test -p index-core` / `cargo check --workspace`). All three pass:
clean compile, all 37 existing `index-core` tests green, no regressions in `cli-indexer` or
`uniques-http-api`. Two named Docker volumes (`cargo-registry-uniques`, `cargo-target-uniques`) now
cache the registry and `target/` across runs, so later checks in this project reuse them instead of
rebuilding from scratch.

### D9 — `cli-indexer build --source cardsdata` wired and validated against all 7 unique sets

[`index-core/src/build.rs`](../index-core/src/build.rs) gained `build_from_cardsdata`, reusing
`apply_card_index`/`write_index_outputs` unchanged (only card *loading* differs from the JSON-crawl
`build()`). [`cli-indexer/src/cli.rs`](../cli-indexer/src/cli.rs)'s `build` subcommand now takes
`--source json|cardsdata` (default `json`, so nothing changes for existing callers).

Running it end-to-end (via Docker, `rust:1.86-bookworm`) against the real CardsData checkout surfaced
and fixed two real data-shape issues the skeleton hadn't accounted for:
- `UniquePrints.ArtistId` isn't always present — made `Option<i64>`, illustrator becomes `None` when
  absent. Investigated further at the user's request: **not a real data gap.** The only rows with an
  empty `ArtistId` are each set's single synthetic `..._B_NE_FOILER_U` placeholder row (`CardFamilyId`
  55, `FactionReference=NE`) — same "not a real card" artifact the JSON-crawl path already filters via
  `is_foiler_path`. No CardsData correction needed; the `Option<i64>` fix stays (the CSV still has to
  parse before that row gets filtered) but there's nothing to reconstruct.
- CardsData carries that same `FOILER` placeholder as a `UniquePrints` row with an unparseable
  `Reference` (`ALT_<SET>_B_NE_FOILER_U`, no faction/family/instance to extract) — `unique_cards()`
  now skips rows that fail `parse_card_reference` instead of failing the build, mirroring how
  `crawl.rs` already skips unparseable paths.

After both fixes, `cli-indexer build --source cardsdata` succeeded on **all 7 CardsData unique sets**
with zero errors (unique-print counts / family counts): ALIZE 1,084,228 / 52, BISE 1,029,202 / 58, CORE
842,268 / 109, COREKS 635,928 / 109, CYCLONE 652,300 / 63, DUSTER 785,600 / 66, EOLE 426,402 / 70. This
answers open question Q1 in practice: across the full dataset, every `PrintEffectFragments` row's
`TriggerId` resolved to a known fragment with a usable `Slot` — no silent drops observed.

### D10 — Non-unique bitmaps don't reuse `StatIndexBuilder`/`FactionIndexBuilder`

Both are coupled to `CompactCardFields` (the unique record: different field set, includes effect
slots the non-unique side doesn't have yet). Rather than generalizing them for 2-3 categorical fields
plus a differently-shaped stat set (5 shared stats, `PERMANENT`/`RESERVE` deferred), `nonunique.rs`
has its own small `NonUniqueIndexBuilder` covering faction/rarity/product/serialized/stats. Same
on-disk shape (one bitmap per category value, 16-bucket stat layout) but a separate, self-contained
implementation — avoids forcing an abstraction across two record types that don't fully overlap, and
keeps this addition zero-risk to the tested unique pipeline (nothing in `bitmap.rs`/`stat_index.rs`/
`faction_index.rs` changed).

### D11 — Non-unique index shipped as a first slice, validated against all 20 sets

[`index-core/src/nonunique.rs`](../index-core/src/nonunique.rs) + `cli-indexer build-nonunique`
(wired in [`cli-indexer/src/cli.rs`](../cli-indexer/src/cli.rs)). No `CardJson`/`CatalogBuilder` reuse
this time — a non-unique print isn't a numbered instance within a family span the way uniques are, so
there's no bit-span logic to plug into; each print gets a flat, sequential `print_index` instead.
Confirmed via grep before designing anything (not assumed, after plan 15 got the effect-slot
assumption wrong initially): `NonUniquePrints.csv` has the **identical header on all 20 sets** — no
per-set dynamic column pivot, just blank cells on rows whose card type doesn't carry a given stat.
This simplified the plan considerably versus the original exploration's assumption.

Validated (Docker, real CardsData) against **all 20 non-unique sets**, zero errors: ALIZE 327, BISE
325, CORE 550, COREKS 510, CYCLONE 353, DUSTER 322, DUSTERCB 222, DUSTEROP 34, DUSTERTOP 24, EOLE 313,
EOLECB 24, EOLEOP 35, EOLETOP 31, FUGUE 318, JUDGE 1, MUSUBI 24, TCS3 6, WCF25 6, WCQ25 6, WCS25 18,
WCS26 11 non-unique prints. Output layout spot-checked for CORE: `cards.bin` is exactly
`550 × 9 = 4,950` bytes, `catalog.json` references look correct, faction/rarity/product/stat bitmap
files present as expected.

One observation, not a bug: some non-unique cards carry `FactionReference=NE` (neutral/factionless) —
not one of the 6 playable factions, so `faction_code_from_reference` maps it to the unknown code (0)
and `NonUniqueIndexBuilder::insert` skips indexing it under any faction bitmap. The card still gets a
`print_index` and stays in the catalog and every other bitmap it qualifies for — it just isn't
findable by a faction filter, which is correct (no faction filter should match a factionless card).

### D12 — `PERMANENT`/`RESERVE` are never indexed, by decision (not an oversight)

Per the user: these two stats only mean anything on Hero cards, and no search over them is planned.
So `NonUniquePrintRow` in [`index-core/src/nonunique.rs`](../index-core/src/nonunique.rs) doesn't read
those CSV columns at all — this is a closed decision, not a deferred TODO to revisit. If that changes
(e.g. a future request to filter/search Hero-specific costs), it plugs in the same way the 5 existing
stats do: one more field on `CompactNonUniqueFields`, one more entry in `STAT_FIELDS`.

### D13 — Merged search shipped as a new endpoint, validated end-to-end including the exclusion rule

`GET /api/v2/search` (not a change to `GET /api/v2/cards` — see plan 19 for why) queries both indexes
from the same `QuerySnapshot`, applying the rule from the original ask: `rarity`/`product`/
`serialized` only exist on the non-unique side, so requesting any of them zeroes the unique branch
entirely (no bitmap even built for it) rather than silently ignoring the filter. `faction` applies to
both.

Loading: `QuerySnapshot.nonunique: Option<Arc<NonUniqueQueryIndex>>`, loaded from the *same*
`IndexStorage` root as the unique index (disk/archive only so far — object_store/http pass `None`
explicitly, same "not yet for source X" shape as http's missing hot-reload support). Best-effort:
`Ok(None)` when `nonunique/catalog.json` isn't present, so index directories built before this lot
keep working unmodified. This only works because `index-core::nonunique::build_nonunique_index` was
changed (before this lot started) to nest its `catalog.json`/`cards.bin`/`manifest.json` under
`nonunique/` too — they used to sit directly under `<SET>/`, which would have collided with the
unique index's own files of the same name once both share one storage root.

Validated via Docker end-to-end (not just `cargo check`): built both indexes for CORE (2,000-card
capped unique index for speed + the full 550-print non-unique index) into one output root, ran the
real `uniques-http-api` binary against it, and curled `/api/v2/search`:
- No filter: `total: 2550` (550 + 2000), non-unique entries first.
- `faction=LY`: filters both branches.
- `rarity=C&limit=200`: `0` unique-kind entries out of 200 returned (confirmed by counting `"kind":
  "unique"` occurrences) — the exclusion rule actually holds under load, not just by code inspection.
- `serialized=true`: empty result, no crash (CORE has no serialized non-unique prints — a genuine
  zero, not a bug).
- `product=A`: 22 alt-art non-unique prints, correctly decoded (faction/rarity/stats all sane).

Response shape uses a `kind`-tagged entry (`"nonunique"` vs `"unique"`) rather than reusing `CardV2` —
the two record types don't share fields yet (no name/effects/artist on the non-unique side per Lot
1's scope). The unique side of this endpoint is also minimal (reference + faction + stats decoded
straight from `CompactCardFields`), not full `CardV2` — reusing `card_v2_from_index` would pull in
locale/idGd plumbing this endpoint doesn't need yet.

### D14 — `page` param added to `GET /api/v2/cards` via `select`/`advance_to`, `withFamilies` now capped

The pre-existing `cursor` scheme in `page_cards_v2` was a full linear scan from the start of the
bitmap on every call (skip-while-`<=`-cursor) — reaching page 12 meant re-walking pages 1-11 each
time, and there was no way to request a page directly without already holding the previous page's
cursor. New `page` param (1-indexed, mutually exclusive with `cursor`, 400 if both given):
`bitmap.select((page-1)*limit)` finds the start by rank directly; the legacy `cursor` path was also
fixed to use `bitmap.iter().advance_to(cursor+1)` instead of the old skip-loop, same behavior, no
longer rescans either. This is the follow-up `cli-indexer/plans/12-bench-query-select-profiling.md`
already flagged ("apply the winning window strategy to `page_cards_v2` after numbers justify it") —
`select`+`advance_to` was that plan's benchmarked candidate.

`families_from_bitmap` (`withFamilies`) previously ignored `limit` entirely and returned every
matching family in one response — the likely cause of the renderer RAM crash `demo-ui/plans/
06-lazy-loading-card-renderer.md` already had to work around client-side. Now always slices by
`page`/`limit` (defaulting to page 1). **This is a behavior change**: existing `withFamilies` callers
that relied on one unpaginated response (demo-ui's "group by family" checkbox) will need a "next
page" control added — not done in this pass, flagged rather than silently left broken.

Validated end-to-end (Docker, real index, not just unit tests): `cursor`-based first page and
`page=1` return the identical first card; `page=2` returns the second card with no prior cursor
needed; `page=9999` (far beyond the result set) returns an empty `cards[]` with no error; `cursor`
and `page` together return 400 with a clear message; `withFamilies` without `page` is now capped at
`limit`, and `withFamilies&page=2` correctly returns fewer/no families once the page runs past the
total family count.

Not done in this pass: `/api/v2/search` (Lot 2) still only returns a first page — paging across two
concatenated bitmaps needs its own design (page N "across both bitmaps combined" isn't a single
`select` call the way one bitmap's page is).

### D15 — `page` added to `/api/v2/search`, correctly straddling the two-bitmap boundary

[`uniques-http-api/plans/21-search-page-pagination.md`](../uniques-http-api/plans/21-search-page-pagination.md).
`/api/v2/search` concatenates two bitmaps virtually (non-unique then unique) — a page can straddle
that boundary, so this needed a variant of Lot 3's `select`/`advance_to` mechanism
(`window_across`) that splits one virtual rank range across two bitmaps, rather than reusing
`window_indices` (which only ever addresses one). Both bitmaps are still reached by direct rank, no
scan of either from its start.

Validated both by unit tests on `window_across` in isolation (entirely-first, straddling,
entirely-second, beyond-both, page-1-matches-no-page-default) and end-to-end against a real built
index: `page=6&limit=100` against a 550-non-unique / 200-unique index landed exactly on the boundary
and returned exactly 50 of each kind; no `page` and `page=1` produced byte-identical `cards[]`; a page
far beyond the combined total returned an empty `cards[]`, no error.

### D16 — `/api/v2/search` gains cost/power, edition, type, subtype, and status filters

Requested filter list, triaged by actual cost before implementing (some looked like simple additions
but needed genuinely new data neither index captured yet):

- **Cost/power** (`mainCost`/`recallCost`/`mountainPower`/`oceanPower`/`forestPower`, exact/`[]`/
  `[gt|gte|lt|lte]`): reuses `CostPredicate`/`parse_cost_predicate` from `/api/v2/cards` directly
  (made `pub(crate)` — same parsing, same error messages, zero duplicated logic) via a new
  `apply_cost_predicate` generic over a `Fn(u8) -> RoaringBitmap` bucket lookup, shared by both
  sides. Applies symmetrically — both card kinds carry these 5 stats.
- **`edition`** (set, e.g. `CORE`/`COREKS`): free on the unique side (`set_bitmaps()` already
  existed). Non-unique needed a new bitmap (`SetReference`, present in the CSV but never read
  before). Note: non-unique builds are still one set at a time — filtering by edition only becomes
  meaningful once multiple non-unique sets are merged into one loaded index, same as unique's own
  `set[]` filter only matters for a merged `ALL_SETS` deployment.
- **`type`**: free on the unique side too, but via a hardcoded rule rather than data — confirmed
  earlier in this thread that a unique is always `CHARACTER`, so `type=CHARACTER` doesn't restrict
  the unique branch and any other value excludes it entirely (added to the same exclusion rule as
  rarity/product/serialized). Non-unique needed a real join: `CardFamilies.CardTypeReference` via
  `CardFamilyId`, previously read by neither `cardsdata.rs` nor `nonunique.rs`.
- **`subtype`**: genuinely new on *both* sides — no existing endpoint filtered by subtype before
  this. Unique side: `build_subtype_bitmaps` derives it entirely from `catalog.json`'s already-present
  `family.card_sub_types` at *load time* — mirrors `build_set_bitmaps` exactly, zero `cli-indexer`
  changes needed. Non-unique side: new `CardFamilySubTypes.csv` join (same join as `type`, so barely
  more work once `type`'s plumbing existed).
- **`banned`/`errated`/`suspended`**: `IsBanned`/`IsErrated`/`IsSuspended` exist in both CSVs but
  were read by neither ingestion path. Added to `CardJson` itself (not just `cardsdata.rs`) so the
  original Equinox JSON-crawl `build()` path picks them up automatically too (the raw JSON already
  has these keys) — not just the CardsData path. New `index-core::status_index` (3 independent
  RoaringBitmaps, since a card can be any combination, unlike the mutually-exclusive rarity/product
  codes) shared by both `build()` and `build_from_cardsdata()` via the same `apply_card_index` both
  already funnel through.

**Open-ended value spaces need an index file, not `has_file` probing.** Faction/rarity/product are a
small fixed code space, so the loader can just try each known code. `set`/`type`/`subtype` are
open-ended (a new set release adds values) — `IndexStorage` has no directory-listing capability by
design (an HTTP/object-store backend can't "ls" a remote root), so `nonunique.rs`'s
`write_named_bitmaps` now also writes an `_index.json` (the list of names that actually got a
bitmap) alongside the `.roar` files, and the loader reads that first — same shape as how
`idgd_catalog.json` already tells the unique loader which idGd bitmap files exist.

**Response shape unchanged**: none of these new filters added fields to `SearchCard` — the ask was
filtering, not more output, so scope stayed there.

Validated end-to-end against a real 5,550-card index (5,000 uniques + 550 non-uniques, CORE): `type=
SPELL` → 123 non-unique, 0 unique (exclusion rule holds); `subtype=ELEMENTAL` → 191 unique + 9
non-unique (positive match confirmed on *both* sides, not just correct exclusion); `edition=CORE` →
total 5,550 (everything); `edition=COREKS` → total 0 (correctly nothing, wrong edition for this
index); `mainCost=2` → 200/200 returned cards actually have `main_cost:2`; `mainCost[gt]=5` → only
values 6-9 present; mixing `mainCost` exact and `[gt]` → `400` with the exact reused
`/api/v2/cards` error message; `banned=true` → empty, no crash (CORE genuinely has none in this
slice).

### D17 — `card_family_id` established as the cross-cutting key; `GET /api/v2/family/{id}` ships

Closes the loop from the very first message of this thread: family name/subtypes stored once,
shared across every unique and non-unique print of that character.

The real content of this slice was establishing a common family identifier — it didn't exist
before. Unique's `FamilyEntry.family_id` is a string (`"AX_04"`) the bit-span addressing scheme
depends on positionally; non-unique read `CardFamilyId` during CSV ingestion (for the `type`/
`subtype` join, D16) but never kept it. Fix: `CardJson` gained `card_family_id: Option<i64>`
(`Some` via `cardsdata.rs`, `None` on the untouched Equinox JSON-crawl path), flowed into
`FamilyMetadata`/`FamilyEntry` (additive, `skip_serializing_if`, old `catalog.json` still parses)
— the string key keeps doing its bit-span job, this just rides alongside it. Non-unique gained a
`family` bitmap dimension (`BTreeMap<String, RoaringBitmap>` keyed by the id as a string, same
`_index.json` mechanism as `set`/`type`/`subtype` since family ids are just as open-ended) — free,
since the CSV ingestion loop already had `record.card_family_id` per print.

New `index-core::family_catalog`: reads `CardFamilies.csv`/`CardFamilySubTypes.csv` *in full*
(every previous reader only ever looked up one family id at a time) into `families.json` — global
data, written into `<out>/<SET>/families.json` by both `build_from_cardsdata` and
`build_nonunique_index` (idempotent, same content either way) so it's reachable from the same
`IndexStorage` root the HTTP server already opens.

`GET /api/v2/family/{id}` returns the catalog entry (name/type/subtypes) plus every matching
unique print (page/limit — a family can have thousands of instances, same RAM concern Lot 3
already fixed for `withFamilies`, no point reintroducing it) and non-unique print (unpaginated —
always a handful of rarities/products per family, not worth the complexity).

Validated end-to-end against a real 5,000-unique/550-non-unique CORE index: `families.json`
generated (245 KB, hundreds of entries with full locale names); `/api/v2/family/302` returned
"Kelon Elemental" — the same family referenced throughout this entire session's manual spot-checks
— with `cardType: CHARACTER`, `subtypes: ["ELEMENTAL"]` (matching earlier ad-hoc observations),
`uniquePrintTotal: 5000` with the first 5 correctly paginated, **and** its 3 non-unique prints
(Common + two Rare variants) in the same response. One name, one set of subtypes, both rarities.

Not in this slice: no `family` filter on `/api/v2/search` (natural follow-up, the bitmap already
exists); no multi-set merge for the family catalog (same caveat as `edition`, D16).

### D18 — `family` filter, non-unique multi-set builds, format/collection, full unique `CardV2` parity

Four remaining open items from D13/D16/D17, done in one pass (the user asked for "everything except
Lot 4"):

- **`family=<id>` filter on `/api/v2/search`**: non-unique reuses the `family` bitmap from D17
  directly. Unique has no precomputed family bitmap (unlike subtype/set) — building one per request
  by scanning `catalog.families` for a `card_family_id` match and inserting its `start_bit..start_bit
  +max_unique_id` range is cheap (family count is small) and needed no new index-core/cli-indexer
  work at all.
- **Non-unique multi-set builds**: `build_nonunique_index` now takes `sets: &[String]` instead of
  one `set: &str`. The key realization: non-unique's flat `print_index` addressing (unlike unique's
  bit-span-per-family scheme) means "merge multiple sets" and "build one set" are *the same
  operation* — read every set's CSV, concatenate, sort once, build. No remapping, no overlap-group
  logic like `merge.rs` needs for uniques. `cli-indexer build-nonunique --set CORE,ALIZE,...`
  (comma-separated, `value_delimiter = ','`) writes directly to `<out>/nonunique/` (out's folder
  name becomes the set name, same convention `merge_indexes` uses), and every print's own
  `SetReference` already lands in the right `set` bitmap with zero extra code — this is what
  actually makes `edition` (D16) a useful filter once more than one set is loaded together.
- **`format`/`collection`**: reuses `resolve_format_filter`/`resolve_collection_filter` from
  `/api/v2/cards` verbatim (made `pub(crate)`, zero duplicated logic) but **unique-branch only** —
  neither concept has a non-unique bitmap representation yet, so the non-unique branch is simply
  unaffected by these two params (not a "doesn't apply" exclusion the way rarity/product is, just
  not built yet). Building this surfaced a real borrow-checker issue: the existing `intersect`
  closure captured `acc` for its whole lifetime, conflicting with format's negated case (a
  subtraction, not an AND) needing to touch `acc` directly — fixed by turning `intersect` into a
  free function taking `&mut Option<RoaringBitmap>` instead of a capturing closure.
- **Full `CardV2` parity for the unique branch**: previously only reference/faction/5 stats:
  `SearchCard::Unique` now flattens a real `CardV2` (reusing `card_v2_from_index` directly, the same
  function `/api/v2/cards` calls) — name/artist/set/subtypes/effects, not a thinner hand-rolled shape.

Validated end-to-end: `build-nonunique --set CORE,ALIZE` produced one combined index (877 = 550 + 327
prints) with both `CORE.roar`/`ALIZE.roar` under `set/` and `manifest.json.source_sets: ["CORE",
"ALIZE"]`; `family=302` returned both kinds; a unique entry from that response carried full name/
artist/set (`"Kelon Elemental"` / `"Zero Wen"` / `"Beyond the Gates"`), not just the old minimal
fields; a collection built from 2 unique references and applied via `/api/v2/search?collection=...`
returned a total of 879 (877 unrestricted non-unique + exactly those 2 unique references, nothing
else) — confirming the "unique-branch-only" scoping holds under load. `format` wasn't separately
end-to-end tested (needs a format manifest fixture) but reuses the identical, already-tested
`/api/v2/cards` resolution path verbatim.

### D19 — Interim effect search on `/api/v2/search`, without waiting for Lot 4

Requested explicitly rather than deferred further: text search for non-unique, id-based (with
duplicate-effect handling) for unique — see
[`uniques-http-api/plans/24-interim-effect-search.md`](../uniques-http-api/plans/24-interim-effect-search.md).

Non-unique gained real effect *text* (not decomposed ids) for the first time — a new
`PrintEffectFragments.csv` × `EffectFragments.csv` join in `nonunique.rs`, working equally well on
today's freeform negative ids since only the resolved text matters, not id stability. Unique's side
needed zero new logic: `effect_slot_bitmap`/`combine_effect_bitmaps`/`parse_effect_slots`/
`validate_idgd_types` already existed for `/api/v2/cards` and were simply exposed (`pub(crate)`) and
called from `search.rs` — `effect[N][matchCount]` (the actual "duplicate effects" ask from the very
first message of this thread) transfers over unchanged.

The two mechanisms are mutually exclusive by design (`effectText` zeroes unique, `effect[N]` zeroes
non-unique) — mixing them in one request zeroes *both* branches rather than erroring, a known,
documented quirk, not fixed in this pass.

Validated against a real 5,000-unique/550-non-unique CORE index: `effectText=fleeting` and
`effectText=FLEETING` return identical results (93, all non-unique) confirming case-insensitivity;
`effect[0][t]=24` returns 200 unique-only results; adding `matchCount=2` shrinks the total as
expected (303 vs the unconstrained count). One early test (`effectText=scout` → 0 results) was
double-checked against the raw generated `effect_text.json` before trusting it — genuinely absent
from CORE's data, not a search bug.

### D20 — Named collections now hold two bitmaps, one per catalog

Discovered while checking `/api/v2/search`'s `collection=` filter for non-unique compatibility:
`POST /api/v2/collection/{id}` used the strict (non-lenient) `build_bitmap_from_ref_strs`, which
only recognizes unique-shaped references (`ALT_<SET>_B_<faction>_<family>_U_<uid>`) — posting even
one non-unique-shaped reference (e.g. `ALT_CORE_B_LY_30_R1`) rejected the *entire* request with a
400, not just that reference. Since a collection needs to represent both card kinds now:

- `index_core::refs_bitmap` gained `build_bitmaps_from_mixed_ref_strs(catalog, Option<&NonUniqueCatalog>, refs)
  -> Result<(RoaringBitmap, RoaringBitmap)>` — a reference that parses as unique-shaped must resolve
  in the unique `Catalog` (unchanged strictness); anything else must match verbatim against
  `NonUniqueCatalog`'s reference list (a new `NonUniqueCatalog::index_for_reference`, exact string
  lookup — non-unique addressing is flat/sequential, so `print_index` *is* the bit, no component
  parsing needed the way unique references need `parse_card_reference` + `catalog.lookup_bit`).
  Still all-or-nothing: any reference matching neither catalog errors the whole call.
- `CollectionStore`'s cached value changed from `Arc<RoaringBitmap>` to `Arc<CollectionBitmaps>`
  (`{ unique, nonunique }`, `uniques-http-api/src/collections/store.rs`) — in-memory only (mini_moka
  cache, no on-disk format), so no migration concern.
- `/api/v2/cards` (unique-only endpoint) reads only `.unique` — unaffected in substance for any
  collection that contains at least one unique reference. One edge case *is* new and was
  deliberately accepted rather than guarded against: a collection made **entirely** of non-unique
  references can now be created (previously rejected at POST time with a 400, since no
  non-unique-shaped reference could ever resolve). Filtering `/api/v2/cards` by such a collection's
  id now succeeds (200) with an empty `.unique` bitmap, i.e. zero results, rather than the id never
  having existed. Confirmed live and knowingly kept as-is — correct per-branch semantics (a
  unique-only endpoint intersected with zero uniques is legitimately empty), just a different
  failure mode (silent empty vs. 400 at creation) than before this change.
- `/api/v2/search`'s `collection=` now intersects **both** branches with their own half of the
  collection: `build_unique_bitmap` uses `.unique` (as before D18), `build_nonunique_bitmap` gained
  a `collections: &CollectionStore` parameter and now intersects `.nonunique` the same way every
  other non-unique filter does. A collection with no non-unique refs simply has an empty
  `.nonunique` bitmap, correctly zeroing that branch when filtered on — matches the semantics every
  other per-branch filter already has.

Validated end-to-end against a real CORE index: a collection posted with one unique ref + one
non-unique ref (`ALT_CORE_B_AX_04_U_1` + `ALT_CORE_B_LY_30_R1`) resolves with `count: 2`;
`/api/v2/search?collection=<id>` then returns exactly those 2 cards (one `kind:"nonunique"`, one
`kind:"unique"`); a unique-only collection still behaves exactly as before (regression-checked); a
still-unparseable reference (`totally_bogus_ref`) still 400s the whole POST, preserving strictness.

## Open questions

- ~~**Q1**: Is `TriggerId` guaranteed non-null on every `PrintEffectFragments` row?~~ Answered by D9:
  yes in practice, across all 7 unique sets — no unresolved slot observed in a full build of each.
- **Q2**: Timeline/scope of CardsData's non-unique effect decomposition + cross-card dedup — gates
  when D6's text-search interim can be retired for non-uniques (Lot 4).
- **Q3**: Confirm `cli-indexer` fetches CardsData via a pinned git clone (tag/commit), the same way it
  clones Equinox repos today, rather than some other artifact mechanism.
