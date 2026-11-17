## Plan 23: shared family catalog + card_family_id (Lot 5, first slice)

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md).

### Goal

Make "family" (a character/card, shared across every unique and non-unique print of it) a
first-class, cross-cutting concept — the last idea from the original ask, and the natural next step
now that filters exist on both sides individually. Concretely: a new `GET /api/v2/family/{id}`
returning the family's name/type/subtypes **once**, plus every matching print (unique and
non-unique) that belongs to it.

### The blocking problem

There's no common family identifier usable on both sides today:
- **Unique**: `FamilyEntry.family_id` is the string `"{faction}_{familyNumber}"`, parsed from the
  print's own reference (`ALT_CORE_B_AX_04_U_1` → `AX_04`) — a positional concept the bit-span
  addressing scheme depends on. It happens to correspond 1:1 to a CardsData `CardFamilyId`, but it
  is not that value.
- **Non-unique**: `CardFamilyId` is read during CSV ingestion (needed for the `type`/`subtype`
  join, see plan 22 / D16) but never persisted anywhere in the built index — it's used transiently
  to resolve bitmaps, then dropped.

So this slice's real content is establishing `CardFamilyId` as a value both sides carry through to
the built index, without touching the unique bit-span mechanism (which must keep using the string
key internally).

### Design

- `CardJson` (and `FamilyMetadata`/`FamilyEntry`) gain `card_family_id: Option<i64>`. Populated by
  `cardsdata.rs` (`Some(print.card_family_id)`); `None` for the original Equinox JSON-crawl path,
  which has no CardsData id to offer. Additive, `skip_serializing_if` — an old `catalog.json` still
  parses.
- Non-unique gains a `family` bitmap dimension — same shape as `set`/`type`/`subtype`
  (`BTreeMap<String, RoaringBitmap>`, `_index.json` + one `.roar` per id, since family ids are as
  open-ended as sets/types), keyed by the family id formatted as a string. Trivial to add: the CSV
  ingestion loop already has `record.card_family_id` per print.
- New `index-core/src/family_catalog.rs`: reads `CardFamilies.csv` + `CardFamilySubTypes.csv`
  (the same join `cardsdata.rs`/`nonunique.rs` each already do once, but this is the first place
  that needs the *whole* referential rather than one family looked up at a time) into
  `Vec<FamilyCatalogEntry { id, name, card_type, subtypes }>`, written as `families.json`. Global,
  not per-set — but written into `<out>/<SET>/families.json` by both `build_from_cardsdata` and
  `build_nonunique_index` (idempotent; whichever ran last wins, same content either way) so it's
  reachable from the same `IndexStorage` root the HTTP server already opens for that set.
- HTTP: `QuerySnapshot` gains `Arc<FamilyCatalog>` (a `BTreeMap<i64, FamilyCatalogEntry>`), loaded
  once at startup from `families.json` (best-effort: `None`/empty if absent, same "old index still
  works" precedent as every other addition this session). New endpoint resolves a family id to its
  catalog entry, its unique members (scan `family_span_groups()` for a matching `card_family_id` —
  family counts are small, no bitmap needed for *this* lookup direction) and non-unique members
  (`nu.family.get(&id.to_string())`).

### Response shape

```json
{
  "familyId": 302,
  "name": { "en_US": "...", "fr_FR": "..." },
  "cardType": "CHARACTER",
  "subtypes": ["ENGINEER", "SPIRIT"],
  "uniquePrints": ["ALT_CORE_B_AX_04_U_1", "ALT_CORE_B_AX_04_U_2", "..."],
  "nonUniquePrints": ["ALT_CORE_B_AX_04_C", "ALT_CORE_P_AX_04_C", "..."]
}
```

Unique families can have thousands of prints — this is exactly the "want to browse a whole
category" case from the original ask, so `uniquePrints` needs the same page/limit treatment as
`withFamilies` (plan 20), not returned unbounded.

### Not in this slice

- No `family` filter added to `/api/v2/search` yet — the ask was the family concept/endpoint, not
  another search parameter; natural follow-up once this lands, reusing the same `family` bitmap.
- No multi-set merge story for the family catalog (same caveat as `edition`, plan 16/D16): today's
  single-set-at-a-time builds mean `families.json` only reflects one set until non-unique gets its
  own `cli-indexer merge` equivalent.
