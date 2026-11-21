# uniques-http-api

HTTP API service for querying the merged `ALL_SETS` index.

There is no runtime dependency from `cli-indexer` on the HTTP API. The HTTP crate depends on **`index-core`** for shared types (`Catalog`, `CompactCardView`, `IdGdCatalog`, bitmap helpers, reference parsing, etc.).


## Local development

- **Config**: copy `uniques-http-api/.env.example` to `.env` (optional shared defaults), then create `.env.local` from `.env.local.template` for local overrides.
- **Defaults**:
  - `PORT=8234` (local dev)
  - `INDEX_PATH=../build/full_index/ALL_SETS`
- **Artist credits** (optional): `ARTISTS__CARDSDATA_CSV_DIR=<CardsData>/data/csv` (or `[artists] cardsdata_csv_dir` in `config/*.toml`) builds the artist credit -> searchable names mapping (`artist[]`, `/api/v2/artists`) from CardsData's `Artists.csv` + `ArtistCreditMembers.csv` at load time, for an index that has no `artist_credits.json` (e.g. the prebuilt production archive). See [`docs/api-spec.md`](../docs/api-spec.md#get-apiv2artists).

Run:

```bash
cargo run -p uniques-http-api
```

Health check:

```bash
curl http://127.0.0.1:8234/healthz
```

## Docker / Cloud Run

### Build the image (embeds `ALL_SETS`)

The repo-root `Dockerfile` expects this directory to exist in the build context:

- `build/full_index/ALL_SETS`

Build:

```bash
docker build -t uniques-http-api .
```

Run locally (container):

```bash
docker run --rm -p 8234:8080 uniques-http-api
```

Notes:
- The server binds `0.0.0.0:$PORT` where `PORT` defaults to `8080` (Cloud Run convention).
- `INDEX_PATH` defaults to `/opt/index/ALL_SETS` in the container image; you can override it if needed.

### Deploy to Cloud Run (public)

Example:

```bash
gcloud run deploy uniques-http-api \
  --source . \
  --allow-unauthenticated \
  --set-env-vars INDEX_PATH=/opt/index/ALL_SETS
```

Then:

```bash
curl "$SERVICE_URL/healthz"
```

## API

See [API spec](../docs/api-spec.md).

## Architecture

See [Architecture](./docs/architecture.md)

