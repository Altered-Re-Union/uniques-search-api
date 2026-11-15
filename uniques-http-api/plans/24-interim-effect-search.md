## Plan 24: interim effect search — text for non-unique, ids for unique (Lot 4 stopgap)

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md) (D6,
D18) and [`23-family-catalog.md`](../../cli-indexer/plans/23-family-catalog.md).

### Goal

Two separate, intentionally-different interim mechanisms on `GET /api/v2/search`, requested
explicitly rather than waiting for Lot 4 (CardsData's non-unique effect decomposition):

- **Non-unique**: `effectText=<substring>` — case/accent-insensitive text search over effect text.
- **Unique**: `effect[N][t|c|o]` + `matchCount` + `effectMode` — the existing id-based mechanism
  from `/api/v2/cards`, wired into `/api/v2/search` (it already existed, just not on this endpoint).

Each is exclusive to its own side — `effectText` zeroes the unique branch, any `effect[N]` zeroes
the non-unique branch. Not one filter with two backends: two mechanisms for two different data
maturity levels (D6), and mixing them zeroes *both* branches (each side's exclusion rule fires
independently) rather than erroring — a self-cancelling but well-formed query, not fixed in this
pass (documented, not silently wrong).

### Non-unique text search: new data, not previously loaded at all

Lot 1 deferred *all* effect data for non-unique. This slice adds it, but only as much as text search
needs — no id catalog, no per-line (M1/M2/M3/echo) structure, no dedup:

- `nonunique.rs`'s CSV reader now also joins `NonUnique/<set>/PrintEffectFragments.csv` (via the
  print's numeric `Id`, not `Reference` — confirmed by reading the real CSV, same shape as the
  unique side's `UniquePrintEffectFragments.csv`) against the shared `EffectFragments.csv` referential,
  concatenating every fragment's text per locale into one blob per print. This works identically
  whether an id is a shared positive template or a freeform negative one (D6) — the join only needs
  the *text* an id resolves to, not a stable/shared id space, so decomposition quality doesn't matter
  here.
- Written as `nonunique/effect_text.json` — **raw**, unnormalized per-print, per-locale text, one
  array entry per `print_index`. Normalization (lowercase, accent-fold) happens once at HTTP load
  time into `NonUniqueQueryIndex.effect_text_search: Vec<Vec<String>>`, reusing
  `normalize_name_for_search` — same load-time-normalization pattern `NameSearchIndex` already uses
  for unique family names, not reinvented.
- `bitmap_for_effect_contains` scans linearly (no bitmap-of-substrings is possible for arbitrary
  text) — fine at non-unique's scale (hundreds to low thousands of prints, not uniques' hundreds of
  thousands).

### Unique id search: no new code, just wiring

`effect_slot_bitmap`/`combine_effect_bitmaps` (bitmap building) and `parse_effect_slots`/
`parse_effect_mode`/`validate_idgd_types` (parsing + type validation) already existed for
`/api/v2/cards` — made `pub(crate)` and called directly from `search.rs`'s `build_unique_bitmap`/
`parse_request`. Identical semantics: `effect[0][t]=24&effect[0][matchCount]=2` requires 2 of the 3
main-effect lines (M1/M2/M3) to contain trigger idGd 24, exactly as it already worked on
`/api/v2/cards`.

### Verification

Built CORE (5,000 uniques + 550 non-uniques) via Docker and confirmed against the real server:
`effectText=fleeting` and `effectText=FLEETING` both return the identical 93 matches, all
`kind:"nonunique"` (case-insensitivity holds, unique branch correctly excluded); `effect[0][t]=24`
returns 200 `kind:"unique"` results and zero non-unique; `effect[0][t]=24&effect[0][matchCount]=2`
returns a smaller, plausible total (303, vs the unconstrained count) — the existing `/api/v2/cards`
matchCount behavior transferred over unmodified. Sanity-checked that "scout" returning zero was a
real data absence (verified via direct grep on the generated `effect_text.json`, not present in
CORE's actual text), not a bug, before trusting the "fleeting" positive match.
