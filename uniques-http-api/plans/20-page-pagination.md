## Plan 20: page-number pagination via bitmap rank/select

See also: [`docs/non-unique-refonte-decisions.md`](../../docs/non-unique-refonte-decisions.md) (Lot 3).

### Goal

Let a client jump directly to page 12 of a result set without having fetched pages 1-11 first, and
cap `withFamilies`' response size (today it returns every matching family in one response — the
likely cause of the card-renderer RAM crash mentioned in
[`demo-ui/plans/06-lazy-loading-card-renderer.md`](../../demo-ui/plans/06-lazy-loading-card-renderer.md)).

### What was actually wrong before this lot

`page_cards_v2`'s existing `cursor` scheme did `for card_index in bitmap.iter() { if card_index <= cursor
{ continue } ... }` — a **linear scan from the start of the bitmap on every call**, skipping past
already-seen elements one by one. Reaching page 12 meant re-walking pages 1-11 every time, and there
was no way to ask for a page directly at all without already holding the previous page's cursor.

`families_from_bitmap` had no pagination whatsoever — `limit` was silently ignored, every matching
family came back in one response.

### Design

- New query param `page` (1-indexed) on `GET /api/v2/cards`, applying to both the normal-paging
  branch and `withFamilies`. Mutually exclusive with `cursor` (400 if both given) — `page` is the new
  direct-access mechanism, `cursor` stays for existing resume-by-last-index clients.
- `page_cards_v2`: when `page` is given, `bitmap.select((page-1) * limit)` finds the starting element
  directly by rank — no scan needed regardless of how deep into the result set that is. When only
  `cursor` is given (legacy), `bitmap.iter().advance_to(cursor + 1)` replaces the old skip-loop — same
  external behavior, but no longer rescans from the start either. Both funnel through one
  `window_indices` helper. `select`/`advance_to` were already benchmarked as the fastest window
  strategy in [`cli-indexer/plans/12-bench-query-select-profiling.md`](../../cli-indexer/plans/12-bench-query-select-profiling.md)
  (that plan's own follow-up note: "apply the winning window strategy to `page_cards_v2` after
  numbers justify it" — this is that follow-up).
- `families_from_bitmap`: now always slices by `page`/`limit` (defaulting to page 1 when `page` is
  omitted) instead of returning everything. **This is a behavior change** for existing `withFamilies`
  callers that relied on getting every match in one response — seed a page ourselves;
  [`demo-ui`](../../demo-ui) is not updated in this pass and will need a "load next page" control
  added to its "group by family" checkbox flow to keep working past `limit` matches. Flagged here
  rather than silently left broken.
- Response: `CardsIter` gains `page: Option<u32>`, echoing the request's `page` when it was used.
  `cursor` behavior is unchanged for the legacy scheme. No `pageCount`/`hasMore` field added — the
  client already gets `total` and can tell it got a full page (`cards.len() == limit` /
  `families.len() == limit`) to know whether to request the next one.

### Not in this pass

- `/api/v2/search` (Lot 2) doesn't have `page` yet — it only returns a first page today (documented
  as a known gap in plan 19). Worth revisiting together with this once both stabilize, since the
  underlying `select`/`advance_to` mechanism is identical; `/api/v2/search`'s two-index concatenation
  makes "page N" mean "page N across both bitmaps combined", which needs a bit more thought than a
  single-bitmap page.
- No change to `page_cards_v2`'s time complexity characterization beyond "no full rescan" — exact
  `select` cost depends on Roaring's internal container layout, not formally bounded here.

### Verification

Existing tests updated for the new `page_cards_v2`/`families_from_bitmap` signatures (both now take
`page: Option<u32>`); new tests added for: direct page access matching sequential cursor paging on
the same data, page beyond the result set returning empty (not an error), and `cursor`+`page`
together being rejected with 400. Docker end-to-end: built an index, curled `/api/v2/cards?page=2` and
compared against manually walking two `cursor` pages to confirm identical results, and curled
`withFamilies&page=2&limit=1` to confirm the response is now capped.
