# HTTP API summary

## `GET /api/v2/cards` Parameters

### Core filters


| Supported | Parameter   | Type / Encoding | Example        | Meaning                                                                                                                                                                                                                                  |
| --------- | ----------- | --------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Yes       | `set[]`     | repeated array  | `set[]=CORE`   | Filter by one or more source set codes (e.g. `CORE`, `COREKS`). Alias: `set=CORE,COREKS` (CSV). OR within listed sets; AND with other filters.                                                                                           |
| Yes       | `faction[]` | repeated array  | `faction[]=AX` | Filter by one or more faction codes.                                                                                                                                                                                                     |
| Yes       | `name`      | string          | `name=Kelon`   | Case-insensitive substring match on character name (any locale). Accented characters match unaccented queries (e.g. `elementaire` matches `Élémentaire`, `boshi` matches `Issun-bōshi`). Whitespace-only values are ignored (no filter). |
| Yes       | `format`    | string          | `format=standard` | Restrict results to a configured format (see [Format filters](#format-filters)). Only one format per request. If the parameter appears more than once, the **last** value wins. Requires `[formats]` in server config; otherwise any `format=` value returns `400 unknown format '{id}'`. |
| Yes       | `collection` | string         | `collection=deck1` | Restrict results to a bitmap previously created via `POST /api/v2/collection/{id}` (see `collections` module). AND with other filters. `422 collection_not_loaded` if the id was never created. |
| Yes       | `ref[]`     | repeated array  | `ref[]=ALT_CORE_B_AX_01_U_1` | Restrict results to an explicit, ad-hoc list of card references given directly in the query string — no `POST /api/v2/collection` round-trip needed. Alias: `ref=REF1,REF2,REF3` (CSV). References are OR'd together, then AND'd with every other active filter (`format`, `collection`, `faction`, etc.), same combination pattern as `collection`. Unknown or malformed references are silently ignored (they just never match) instead of causing an error. |


### Format filters

When the server is configured with a `[formats]` section, format definitions are loaded from a
manifest-driven source — either a directory on disk or a `manifest.json` served over HTTP. Each
format is a JSON file listed in `manifest.json`:

```json
[
  { "id": "standard", "path": "standard.json", "version": 1 },
  { "id": "draft", "path": "draft.json", "version": 2 }
]
```

Each format JSON must match its manifest entry on `id` and `version`. A format uses **either**
include mode **or** exclude mode (not both):

| Mode | Fields | Query effect |
| ---- | ------ | ------------ |
| Include | `included_refs` (card reference strings) | AND with other filters |
| Exclude | `excluded_sets` (set codes), `excluded_refs` (card references) | AND NOT with other filters |

Example include format:

```json
{
  "id": "standard",
  "version": 1,
  "included_refs": ["ALT_CORE_B_AX_01_U_1", "ALT_CORE_B_BR_02_U_3"]
}
```

Example exclude format:

```json
{
  "id": "no-coreks",
  "version": 1,
  "excluded_sets": ["COREKS"],
  "excluded_refs": []
}
```

**Errors**

| Status | Condition |
| ------ | --------- |
| `400` | Unknown format id, or `[formats]` not configured |
| `500` | Known format id but that format failed to load at startup/reload (`format failed to load`) |

Formats apply to `GET /api/v2/cards` and `GET /api/v2/effects/filtered` (same filter pipeline).

**Server config** (`config/default.toml` or env):

```toml
[formats]
# reload_interval_secs = 60  # omit or 0 = no hot-reload

[formats.source]
type = "disk"
path = "./formats"
```

Or pull the manifest (and sibling format files) over HTTP instead of from disk:

```toml
[formats.source]
type = "http"
manifest_url = "https://altered-reunion-formats-prod.s3.fr-par.scw.cloud/manifest.json"
```

Each manifest entry's `path` is resolved relative to `manifest_url` (i.e. as a sibling file next
to `manifest.json`). `FORMATS_PATH` env overrides `formats.source.path` (disk source only).

### Numeric and stat filters


| Supported | Parameter                                                           | Type / Encoding         | Example                         | Meaning                                  |
| --------- | ------------------------------------------------------------------- | ----------------------- | ------------------------------- | ---------------------------------------- |
| Yes       | `mainCost`                                                          | exact integer           | `mainCost=3`                    | Exact main cost.                         |
| Yes       | `mainCost[gt]` / `mainCost[gte]` / `mainCost[lt]` / `mainCost[lte]` | ranged integer          | `mainCost[gte]=3`               | Main cost greater/less than comparisons. |
| Yes       | `mainCost[]`                                                        | repeated array          | `mainCost[]=2&mainCost[]=3`     | Match any of several exact values.       |
| Yes       | `recallCost` / `recallCost[...]` / `recallCost[]`                   | integer or ranged/array | `recallCost[lte]=1`             | Recall cost filter.                      |
| Yes       | `oceanPower` / `oceanPower[...]` / `oceanPower[]`                   | integer or ranged/array | `oceanPower[]=0&oceanPower[]=1` | Ocean power filter.                      |
| Yes       | `mountainPower` / `mountainPower[...]` / `mountainPower[]`          | integer or ranged/array | `mountainPower[gt]=5`           | Mountain power filter.                   |
| Yes       | `forestPower` / `forestPower[...]` / `forestPower[]`                | integer or ranged/array | `forestPower=2`                 | Forest power filter.                     |


### Effect filters


| Supported | Parameter      | Type / Encoding                    | Example           | Meaning                                    |
| --------- | -------------- | ---------------------------------- | ----------------- | ------------------------------------------ |
| Yes       | `effect[0][t]` | integer id or comma-separated list | `5`, `1,5,12`     | Ability trigger idGd for slot 0.           |
| Yes       | `effect[0][c]` | integer id or comma-separated list | `3`, `3,16,199`   | Ability condition idGd for slot 0.         |
| Yes       | `effect[0][o]` | integer id or comma-separated list | `42`, `42,94,601` | Ability output idGd for slot 0.            |
| Yes       | `effect[0][matchCount]` | `1` (default), `2` or `3` | `2`               | Require `matchCount` abilities matching the predicates. |
| Yes       | `effect[1][t]` | integer id or comma-separated list | `24`              | Ability trigger idGd for slot 1.           |
| Yes       | `effect[1][c]` | integer id or comma-separated list | `191`             | Ability condition idGd for slot 1.         |
| Yes       | `effect[1][o]` | integer id or comma-separated list | `90`              | Ability output idGd for slot 1.            |
| Yes       | `effectMode`   | enum                               | `and`, `or`       |                                            |
| Yes       | `support[t]`   | integer id or comma-separated list | `support[t]=24`   | Ability trigger idGd for support effect.   |
| Yes       | `support[c]`   | integer id or comma-separated list | `support[c]=191`  | Ability condition idGd for support effect. |
| Yes       | `support[o]`   | integer id or comma-separated list | `support[o]=90`   | Ability output idGd for support effect.    |


If multiple values are specified for a trigger, condition or output, a predicate is created that matches ANY of these values (OR).
If multiple effects are specified, the `effectMode` determines if the card must match at least one of them (`or`) or all of them (`and`).

### Response options


| Supported | Parameter      | Type | Example        | Meaning                                                                                                                                                                           |
| --------- | -------------- | ---- | -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Yes       | `withFamilies` | flag | `withFamilies` | On the **first** request only (`cursor` omitted): add `families[]` and replace normal `cards[]` paging with one full card per matching family (see Response). `limit` is ignored. |
| Yes       | `limit`        | int  | `limit=50`     | Page size for `cards[]` when `withFamilies` is absent (default 50, max 200).                                                                                                      |
| Yes       | `cursor`       | int  | `cursor=10516` | Resume `cards[]` paging after this `card_index`. When set, `withFamilies` is ignored.                                                                                             |


## `GET /api/v2/cards` Response

The response is a JSON object with `iter` (match totals and paging), and `cards` (a page of full card objects). `iter.total` is the number of matching cards across the whole query; `iter.cursor` is present when more pages exist.

### Default (no `withFamilies`)

```
{
  iter: {
    total: 51513,
    cursor: 10516,
  },
  cards: [
    {
      reference: "ALT_COREKS_B_AX_05_U_161",
      name: {
        en_US: "Ayxas, Repented Tyrant",
        fr_FR: "Ayxas, Tyran Repenti"
        ...
      },
      artist: "Artist Name",
      set: {
        reference: "COREKS",
        name: "Beyond the Gates - KS Edition",
        code: "BTG"
      },
      cardSubTypes: [
        {
          reference: "NOBLE",
          name: { en_US: "Noble", fr_FR: "Noble", ... }
        }
      ],
      mainCost: 2,
      recallCost: 3,
      forestPower: 1,
      mountainPower: 6,
      oceanPower: 3,
      faction: {
        code: "AX",
        name: "Axiom"
      },
      mainEffect: {
        en_US: "{R} If there's a card in your Landmarks: Target Character in play or in Reserve gains 2 boosts.  When I leave the Expedition zone — []You may [Augment] target card in play or in Reserve.",
        fr_FR: "{R} S'il y a au moins une carte dans vos Repères : Un Personnage ciblé en jeu ou en Réserve gagne 2 boosts.  Lorsque je quitte la zone d'Expédition — [] Vous pouvez [AUGMENT] une carte ciblée en jeu ou en Réserve."
        de_DE: "...",
        es_ES: "...",
        it_IT: "..."
      },
      echoEffect: {
        en_US: "{D} : []Pay {1} less for the next card you play this turn.",
        fr_FR: "{D} : [] Payez {1} de moins pour la prochaine carte que vous jouez ce tour-ci.",
        de_DE: "...",
        es_ES: "...",
        it_IT: "..."
      }
    },
    {
      reference: "ALT_COREKS_B_BR_51_U_3467"
      ...
    },
  ]
}
```

### With `withFamilies` (first page only)

Requires `withFamilies` and no `cursor`. Adds `families[]` (omitted when `cursor` is set).


| Field       | Type    | Description                                                                             |
| ----------- | ------- | --------------------------------------------------------------------------------------- |
| `familyId`  | string  | Logical family id (`{faction}_{number}`). CORE+COREKS overlap counts are merged per id. |
| `count`     | integer | Matching cards in that family.                                                          |
| `reference` | string  | First matching card in the family (lowest `card_index`; typically COREKS).              |
| `name`      | object  | Localized character name (locale → string).                                             |


`families[]` is not paginated. `**cards[]` contains only the full `CardV2` for each `families[].reference**` (same order; one card per family). `limit` and `iter.cursor` do not apply on this response — use a follow-up request **without** `withFamilies` (and optional `cursor`) to page through all matching prints.

```
{
  iter: {
    total: 51513,
  },
  families: [
    {
      familyId: "AX_05",
      count: 42,
      reference: "ALT_COREKS_B_AX_05_U_1",
      name: { en_US: "Ayxas, Repented Tyrant", fr_FR: "..." }
    },
    {
      familyId: "BR_51",
      count: 8,
      reference: "ALT_COREKS_B_BR_51_U_1",
      name: { en_US: "...", fr_FR: "..." }
    }
  ],
  cards: [
    { reference: "ALT_COREKS_B_AX_05_U_1", name: { ... }, artist: "...", set: { ... }, ... },
    { reference: "ALT_COREKS_B_BR_51_U_1", name: { ... }, artist: "...", set: { ... }, ... }
  ]
}
```

## `GET /api/v2/card/{reference}`

Look up a single card by its full reference id (same object shape as one element of `cards[]` in the search response).


|           |                                                   |
| --------- | ------------------------------------------------- |
| **Path**  | `{reference}` — e.g. `ALT_CYCLONE_B_BR_77_U_1787` |
| **Query** | `debug_bga_trigram` (optional, same as search)    |



| Status  | Meaning                                                                            |
| ------- | ---------------------------------------------------------------------------------- |
| **200** | One `CardV2` object at the JSON root (`reference`, `name`, `set`, `mainEffect`, …) |
| **400** | Reference does not match `ALT_<SET>_B_<faction>_<family>_U_<uid>`                  |
| **404** | Unknown family, UID beyond family span, or slot not indexed                        |


Example:

```
GET /api/v2/card/ALT_COREKS_B_AX_05_U_161
```

```json
{
  "reference": "ALT_COREKS_B_AX_05_U_161",
  "name": { "en_US": "Ayxas, Repented Tyrant", "fr_FR": "..." },
  "artist": "Artist Name",
  "set": { "reference": "COREKS", "name": "Beyond the Gates - KS Edition", "code": "BTG" },
  "cardSubTypes": [{ "reference": "NOBLE", "name": { "en_US": "Noble", ... } }],
  "mainCost": 2,
  "recallCost": 3,
  "forestPower": 1,
  "mountainPower": 6,
  "oceanPower": 3,
  "faction": { "code": "AX", "name": "Axiom" },
  "mainEffect": { "en_US": "...", "fr_FR": "..." },
  "echoEffect": { "en_US": "...", "fr_FR": "..." }
}
```

## GET `/api/v2/effects` Response

This endpoint takes no parameters.

It returns a list of the Effect parts available for filtering. Each part has its raw `text` and a
readable `formattedText` for the trigger / condition / output pickers, both keyed by locale. The
body is built once when the index loads, and also gzipped once then: it is sent gzipped when the
request has `Accept-Encoding: gzip`.

`formattedText` is a [Markdown](#formatted-text-markdown) string per locale, built from `text` as follows:

| Raw `text` | `formattedText` (`fr_FR`) | `formattedText` (`en_US`) |
| ---------- | ------------------------- | ------------------------- |
| `[]` (condition) | `Sans condition` | `No condition` |
| `[]` (trigger or output) | `""` (empty) | `""` (empty) |
| `{H}` | `Joué depuis la Main` | `Played from Hand` |
| `{R}` | `Joué depuis la Réserve` | `Played from Reserve` |
| `{J}` | `Joué de partout` | `Played from anywhere` |
| `{T}` | `Épuisez-moi` | `Exhaust me` |
| `{D}` or `{D} :` | `Défaussez-moi de la Réserve` | `Discard me from Reserve` |
| `{I} À Midi —` | `À Midi —` (leading `{I}` removed) | `At Noon —` |
| `[RESUPPLY_LOW].` | `**Ravitaillez**.` | `**Resupply**.` |
| `Si j'ai au moins 1 boost :` | `Si j'ai au moins 1 boost` | `If I have at least 1 boost` |

- Symbol labels exist in `de_DE`, `en_US`, `es_ES`, `fr_FR` and `it_IT`; other locales get English.
- `[CODE]` keywords are printed from the index's `keywords.json` (see
  [ALL_SETS index format](ALL_SETS-index-format.md#keywordsjson)). A code missing in a locale
  falls back to its `en_US` name; a code missing from the file is left as written (`[CODE]`).
- Non-breaking and repeated spaces become one space, including inside keyword names
  (`Tough\u00a01` → `Tough 1`); trailing spaces and `:` are removed.
  Symbols inside a sentence (`{J} Piochez une carte.`) are kept.

```
{
  triggers: [
    {
      idGd: 1,
      text: {
        en_US: "{R}",
        fr_FR: "{R}",
        de_DE: ...
        es_ES: ...
        it_IT: ...
      },
      formattedText: {
        en_US: "Played from Reserve",
        fr_FR: "Joué depuis la Réserve",
        ...
      },
      isEcho: false,
      isMain: true,
    },
    {
      idGd: 2,
      text: {
        en_US: "When an opponent draws one or more cards or does [RESUPPLY_T] —",
        fr_FR: "Lorsqu'un adversaire pioche au moins une carte ou [RESUPPLY_T] —",
        de_DE: ...
        es_ES: ...
        it_IT: ...
      }
    },
    ...
  ],
  conditions: [
    {
      idGd: 166,
      text: {
        en_US: "If you control two or more Plants other than me:",
        fr_FR: ...
        de_DE: ...
        es_ES: ...
        it_IT: ...
      },
      isEcho: false,
      isMain: true,
    },
    {
      idGd: 167,
      text: {
        en_US: "If there are three or more base statistics of 0 among Characters you control:",
        fr_FR: ...
        de_DE: ...
        es_ES: ...
        it_IT: ...
      }
    },
    ...
  ],
  output: [
    {
      idGd: 193,
      text: {
        en_US: "[AFTER_YOU].",
        fr_FR: ...
        de_DE: ...
        es_ES: ...
        it_IT: ...
      },
      formattedText: {
        en_US: "**After You**.",
        fr_FR: "**Après vous**.",
        ...
      },
      isEcho: false,
      isMain: true,
    },
    ...
  ]
}
```

### Formatted text Markdown

`formattedText` values are [CommonMark](https://commonmark.org/) strings on one line, ready to
pass to a Markdown renderer:

```json
"Lancez un dé. Sur 4+, **Ravitaillez**."
```

- Keywords are the only formatting, in bold (`**Ravitaillez**`).
- Characters that Markdown would interpret are escaped with a backslash so the text displays as
  written: `\`, `*`, `_`, `` ` ``, `[`, `]`, `<`, `>`, `~` anywhere, and a line start read as a
  heading or a list item (`# `, `- `, `+ `, `1. `, `1) `). A keyword code missing from
  `keywords.json` is sent as `\[CODE\_NAME\]` and displayed as `[CODE_NAME]`.
- Symbols (`{J}`, `{1}`...) are plain text.

## `GET /api/v2/effects/filtered`

Per-combobox autocomplete narrowing. Given the current filters and which effect box the user is
editing, returns just the idGds for that box that would still yield an ability that actually exists.
Use `/api/v2/effects` once for labels/text; use this endpoint to narrow the candidate ids as filters
are added. Presence-only, ids-only. Typically responds in a few milliseconds.

### Parameters

The client sends its **full current filter state** exactly as it would to `/api/v2/cards`
(`effect[N][...]`, `support[...]`, `effectMode`, `faction`, `set`, `mainCost`/`recallCost`, `name`) —
**including the group being edited** — plus one extra param:


| Supported | Parameter | Type / Encoding | Example             | Meaning                                                                                                                                                                                                                                                 |
| --------- | --------- | --------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Yes       | `editing` | `<part>:<slot>` | `editing=trigger:0` | The box being edited. `part` is `trigger`, `condition`, or `output`. `slot` is a main-effect slot index (`0`, `1`, ... matching the `effect[N]` indices) or the literal `support` for the echo/support slot. Examples: `condition:1`, `output:support`. |


Semantics:

- The trigger/condition/output of one group form a single ability that must co-occur on the **same
line**. Candidates are returned only if they co-occur, on the same line, with the group's other
two boxes (and satisfy all the other filters). Main slots search lines M1/M2/M3; `support` searches
the echo line.
- The server **excludes the edited group** (identified by `editing`'s slot) from the search space,
so the box's own current value never filters out the alternatives the user might pick instead.
- If `slot` refers to a group not present in the filters (a brand-new, empty group), there are no
co-constraints and nothing to exclude — candidates are narrowed by the remaining filters only.
- Guarantee: every returned id, when set in that box and posted to `/api/v2/cards`, yields >= 1 card
(exact for the default `effectMode=and`; `or` across multiple groups is best-effort).

### Response

Ids only (the client already has localized text from its initial `/api/v2/effects` load):

```
{
  "editing": "trigger:0",
  "idGds": [1, 5, 24, ...]
}
```


| Status  | Meaning                                                                                     |
| ------- | ------------------------------------------------------------------------------------------- |
| **200** | `{ editing, idGds }`                                                                        |
| **400** | Missing/invalid `editing` (bad `part` or `slot`), or a co-constraint idGd of the wrong type |


