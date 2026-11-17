## Plan 19: merged unique + non-unique search (first slice)

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md) (D1-D12),
[`cli-indexer/plans/15-cardsdata-csv-ingestion.md`](../../cli-indexer/plans/15-cardsdata-csv-ingestion.md)
and [`16-nonunique-index.md`](../../cli-indexer/plans/16-nonunique-index.md) (the two indexes this
merges).

### Goal / scope cut

Lot 2 from the decision log: query both indexes in parallel and concatenate, non-unique results
first, capped at the requested `limit` — the behavior asked for in the very first message of this
thread. New, additive endpoint `GET /api/v2/search`, **not** a change to the existing, heavily-tested
`GET /api/v2/cards` — that endpoint's `CardsRequest`/`build_bitmap`/`CardV2` machinery is unique-only
by design (effect filters, families, formats, collections) and forcing it to also emit non-unique
rows would mean reworking its response shape for every existing consumer. A new endpoint sidesteps
that risk entirely, consistent with every other lot so far (pure addition, zero changes to tested
code paths).

First-slice filter set, deliberately small: `faction` (applies to both indexes), `rarity`/`product`/
`serialized` (non-unique only — see the exclusion rule below), `limit`. Cost/power predicates,
`format`/`collection` interplay, and anything effect-related are not in this slice — same mechanical
shape as the existing filters, follow-up work once the merge mechanism itself is proven.

Pagination is intentionally naive here: only the first page (no `cursor`) is correct. Lot 3
(page-number pagination via bitmap `rank`/`select`) is where cross-index paging gets solved properly;
this slice doesn't pretend otherwise.

### Loading

Both indexes are read from the **same** `IndexStorage` (disk or archive; object_store/http not
wired for non-unique yet, same "not yet for source X" precedent as hot-reload-over-http). The
non-unique index's files already live under `nonunique/` relative to the same storage root as the
unique index's `catalog.json`/`cards.bin` (see `index-core/src/nonunique.rs`'s `build_nonunique_index`
— this only works because that function was just changed to nest catalog/cards.bin/manifest under
`nonunique/` too, avoiding a filename collision with the unique index's own `catalog.json`/`cards.bin`
at the storage root).

Loading is **best-effort**: if `nonunique/catalog.json` isn't present at the storage root,
`QuerySnapshot.nonunique` is `None` and `/api/v2/search` behaves as unique-only — existing index
directories built before this lot keep working unmodified, no forced rebuild.

`QuerySnapshot` gains `nonunique: Option<Arc<NonUniqueQueryIndex>>`. Hot-reload (`commit_if_newer`)
and formats-only reload both carry the current `nonunique` value forward unchanged — this slice
doesn't add non-unique hot-reload, mirroring how `index.source = http` doesn't support hot-reload
either.

### Filter compatibility (the "understand which fields are unique-only" rule from the original ask)

`rarity`, `product`, and `serialized` only exist on the non-unique side — a unique print is always
rarity "Unique", has no product code, and isn't independently serialized the same way. So: **if the
request includes any of `rarity`/`product`/`serialized`, the unique branch is skipped entirely**
(empty result, no bitmap even built) rather than silently ignoring the filter. `faction` applies
symmetrically to both indexes since both track it.

This is the inverse of the direction actually named in the opening ask ("ne renvoyer aucun résultat
d'unique lorsqu'un filtre de recherche sur un autre type est appliqué") — restated precisely: a filter
whose value space doesn't include uniques (rarity/product/serialized) zeroes the unique branch. The
symmetric case (a unique-only filter zeroing the non-unique branch — e.g. a future effect-id filter)
doesn't exist yet in this slice since no unique-only filter is exposed on `/api/v2/search` yet; the
rule is written generically in the code so it extends the same way once one is added.

### Response shape

`cards[]` entries are tagged by `kind` (`"unique"` or `"nonunique"`) rather than reusing `CardV2` — the
two record types don't carry the same fields (no name/effects/artist for non-unique yet, per Lot 1's
scope cut) and forcing a shared shape would mean fabricating placeholder values. A consumer switches
on `kind`.

```json
{
  "iter": { "total": 542 },
  "cards": [
    { "kind": "nonunique", "reference": "ALT_CORE_B_LY_30_R1", "faction": "LY", "rarity": "R",
      "product": "B", "serialized": false, "mainCost": 3, "recallCost": 3,
      "mountainPower": 0, "oceanPower": 0, "forestPower": 0 },
    { "kind": "unique", "reference": "ALT_CORE_B_AX_04_U_3463", "faction": "AX",
      "mainCost": 2, "recallCost": 2, "mountainPower": 4, "oceanPower": 0, "forestPower": 2 }
  ]
}
```

Non-unique entries are built directly from `CompactNonUniqueFields` (no family/name lookup yet, per
Lot 1). Unique entries in this slice are also minimal (reference + faction + stats, decoded straight
from `CompactCardFields`) rather than the full `CardV2` (name/effects/artist) — reusing
`card_v2_from_index` would pull in the `idgd_by_id`/locale plumbing this endpoint doesn't otherwise
need; full `CardV2` parity for the unique side of `/api/v2/search` is a follow-up, not this slice.

### Implementation

- [`uniques-http-api/src/index/loader/nonunique.rs`](../../uniques-http-api/src/index/loader/nonunique.rs):
  `NonUniqueQueryIndex` + `load_nonunique_index(storage: &impl IndexStorage) -> Result<Option<...>>`,
  probing the known finite code spaces (6 factions, 3 rarities, 3 products, 5 stats × 16 buckets) via
  `IndexStorage::has_file` rather than directory listing (the trait has no listing capability, by
  design — an HTTP/object-store backend can't "ls" a remote root either).
- `QuerySnapshot.nonunique: Option<Arc<NonUniqueQueryIndex>>`, wired through `build_app_state` (disk/
  archive only — http/object_store pass `None` explicitly for now) and both reload paths.
- New module `uniques-http-api/src/http/api/search.rs`: parses the small filter set, builds one
  `RoaringBitmap` per index (skipping the unique one per the exclusion rule above), decodes up to
  `limit` entries non-unique-first, returns the tagged response.
- Router: `GET /api/v2/search` added to `uniques-http-api/src/http/api.rs`'s existing router merge,
  alongside (not replacing) `/api/v2/cards`.

### Verification

Build both indexes for one real CardsData set into the same output root (`cli-indexer build
--source cardsdata --set CORE --out X` then `cli-indexer build-nonunique --root <cardsdata> --set CORE
--out X`), start `uniques-http-api` against `X/CORE` via Docker, and curl `/api/v2/search` with a few
filter combinations — including one with `rarity=` set, to confirm the unique branch actually comes
back empty rather than merely unfiltered.
