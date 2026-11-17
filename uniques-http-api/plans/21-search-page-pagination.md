## Plan 21: page pagination for `GET /api/v2/search`

See also: [`19-merged-search.md`](19-merged-search.md) (Lot 2) and
[`20-page-pagination.md`](20-page-pagination.md) (Lot 3 on `/api/v2/cards`, same underlying
mechanism, single bitmap only).

### Goal

`/api/v2/search` shipped in Lot 2 with only a first page (documented gap in plan 19/D13). This adds
`page` (1-indexed), matching `/api/v2/cards`'s param name and semantics.

### Why this isn't just "reuse `window_indices` twice"

`/api/v2/search` concatenates two bitmaps — non-unique results, then unique — into one virtual
ordered list. A page can **straddle the boundary**: e.g. with `limit=50` and 30 non-unique matches
total, page 1 is all non-unique, but page 2 (ranks 50-99) is entries 20-49 of the non-unique bitmap
(ranks 20-29, only 10 of them exist) *plus* entries 0-39 of the unique bitmap. `window_indices`
(plan 20) only ever addresses one bitmap; this needed a variant that splits one virtual rank range
across two.

### Design

New helper `window_across(first, second, start_rank, limit)`: computes how much of `[start_rank,
start_rank+limit)` falls in `first`'s rank space (`select`+`advance_to`, same as plan 20), then
whatever's left comes from `second` at rank `start_rank - first.len()` (clamped to 0), also via
`select`+`advance_to`. Both bitmaps are still addressed by direct rank — no scan of either from its
start, and no need to have fetched previous pages, regardless of where in the concatenation the page
falls.

`start_rank = (page - 1) * limit`. One overflow guard worth calling out: `first.len()` is a real
bitmap cardinality (always representable as a rank), but `start_rank` comes straight from
`page * limit` with no relationship to either bitmap's actual size — a large `page` could push
`start_rank - first.len()` past `u32::MAX`. That's rejected (treated as "no match" rather than
truncated) via `u32::try_from`, instead of letting an `as u32` cast silently wrap and risk returning
data at the wrong rank.

Response: `SearchIter` gains `page: Option<u32>`, echoed back when the request used it — same
convention as `CardsIter` in plan 20.

### Verification

Docker end-to-end against a real built index (both sub-indexes for one set): requested a page known
to straddle the non-unique/unique boundary (chosen from the two bitmaps' actual sizes) and confirmed
the response mixes both `kind`s in the expected proportion; confirmed `page=1` with no `page` param
produces identical results (default behavior unchanged); confirmed a page far beyond the combined
total returns an empty `cards[]`, not an error.
