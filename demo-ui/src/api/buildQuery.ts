import { parseCostInput } from './parseCostInput';
import type { EffectRegion } from './effectCatalogOptions';
import type {
  BuildQueryResult,
  EffectPart,
  EffectSlot,
  FilterState,
} from '../types';

function slotHasValues(slot: EffectSlot): boolean {
  return slot.t.trim() !== '' || slot.c.trim() !== '' || slot.o.trim() !== '';
}

export function activeEffectSlotCount(state: FilterState): number {
  return state.effects.filter(slotHasValues).length;
}

/**
 * Index a main effect slot would occupy in the compacted `effect[N]` numbering
 * (empty UI slots are skipped). Matches the renumbering done in `appendFilterParams`.
 */
function compactedSlotIndex(state: FilterState, uiIndex: number): number {
  let count = 0;
  const end = Math.min(uiIndex, state.effects.length);
  for (let i = 0; i < end; i += 1) {
    if (slotHasValues(state.effects[i])) {
      count += 1;
    }
  }
  return count;
}

function appendCostParams(
  params: URLSearchParams,
  key: string,
  values: number[],
): void {
  if (values.length === 0) {
    return;
  }
  if (values.length === 1) {
    params.set(key, String(values[0]));
    return;
  }
  for (const value of values) {
    params.append(`${key}[]`, String(value));
  }
}

function appendEffectField(
  params: URLSearchParams,
  slotIndex: number,
  field: 't' | 'c' | 'o',
  raw: string,
): void {
  const value = raw.trim();
  if (!value) {
    return;
  }
  params.set(`effect[${slotIndex}][${field}]`, value);
}

export type BuildQueryOptions = {
  cursor?: number;
};

type AppendFilterParamsResult = {
  handCostError?: string;
  reserveCostError?: string;
};

/**
 * Append the shared `/api/v2/cards`-style filter params (effects, effectMode,
 * support, factions, sets, name, costs) used by both the cards query and the
 * filtered-effects query. Cost parsing is best-effort: invalid costs are
 * skipped and reported via the return value rather than throwing.
 */
function appendFilterParams(
  params: URLSearchParams,
  state: FilterState,
): AppendFilterParamsResult {
  let handCostError: string | undefined;
  let reserveCostError: string | undefined;

  // Skip empty UI slots; compact active slots to effect[0], effect[1], …
  let effectIndex = 0;
  for (const slot of state.effects) {
    if (!slotHasValues(slot)) {
      continue;
    }
    appendEffectField(params, effectIndex, 't', slot.t);
    appendEffectField(params, effectIndex, 'c', slot.c);
    appendEffectField(params, effectIndex, 'o', slot.o);
    if (slot.matchCount !== 1) {
      params.set(`effect[${effectIndex}][matchCount]`, String(slot.matchCount));
    }
    effectIndex += 1;
  }

  if (activeEffectSlotCount(state) >= 2 && state.effectMode === 'or') {
    params.set('effectMode', 'or');
  }

  if (slotHasValues(state.support)) {
    const supportFields: Array<['t' | 'c' | 'o', string]> = [
      ['t', state.support.t],
      ['c', state.support.c],
      ['o', state.support.o],
    ];
    for (const [field, raw] of supportFields) {
      const value = raw.trim();
      if (value) {
        params.set(`support[${field}]`, value);
      }
    }
  }

  for (const faction of state.factions) {
    params.append('faction[]', faction);
  }

  for (const set of state.sets) {
    params.append('set[]', set);
  }

  const search = state.search.trim();
  if (search) {
    params.set('q', search);
  }

  const format = state.format.trim();
  if (format) {
    params.set('format', format);
  }

  const collection = state.collection.trim();
  if (collection) {
    params.set('collection', collection);
  }

  if (state.handCost.trim()) {
    const parsed = parseCostInput(state.handCost);
    if (!parsed.ok) {
      handCostError = parsed.error;
    } else {
      appendCostParams(params, 'mainCost', parsed.values);
    }
  }

  if (state.reserveCost.trim()) {
    const parsed = parseCostInput(state.reserveCost);
    if (!parsed.ok) {
      reserveCostError = parsed.error;
    } else {
      appendCostParams(params, 'recallCost', parsed.values);
    }
  }

  return { handCostError, reserveCostError };
}

export function buildQuery(
  state: FilterState,
  options?: BuildQueryOptions,
): BuildQueryResult {
  const params = new URLSearchParams();

  const { handCostError, reserveCostError } = appendFilterParams(params, state);

  if (handCostError || reserveCostError) {
    return { ok: false, handCostError, reserveCostError };
  }

  const limitRaw = state.limit.trim();
  const limitParsed =
    limitRaw === '' ? 50 : Number.parseInt(limitRaw, 10);
  const limit = Math.min(
    200,
    Math.max(1, Number.isNaN(limitParsed) ? 50 : limitParsed),
  );
  params.set('limit', String(limit));

  if (state.withFamilies && options?.cursor === undefined) {
    params.set('withFamilies', '');
  }

  if (state.debugBgaTrigram) {
    params.set('debug_bga_trigram', '');
  }

  if (options?.cursor !== undefined) {
    params.set('cursor', String(options.cursor));
  }

  return { ok: true, params };
}

export function buildQueryString(
  state: FilterState,
  options?: BuildQueryOptions,
): BuildQueryResult {
  return buildQuery(state, options);
}

export function buildRequestPath(
  state: FilterState,
  options?: BuildQueryOptions,
): BuildQueryResult & {
  path?: string;
  queryString?: string;
} {
  const result = buildQuery(state, options);
  if (!result.ok) {
    return result;
  }
  const qs = result.params.toString();
  return {
    ok: true,
    params: result.params,
    queryString: qs,
    path: qs ? `/api/v2/cards?${qs}` : '/api/v2/cards',
  };
}

/** Which combobox is being edited, used to build the `editing=<part>:<slot>` param. */
export type FilteredEffectsTarget = {
  region: EffectRegion;
  /** UI index in `state.effects` (ignored for the `echo`/support region). */
  slotIndex: number;
  part: EffectPart;
};

const EFFECT_PART_NAMES: Record<EffectPart, string> = {
  t: 'trigger',
  c: 'condition',
  o: 'output',
};

/**
 * Build the query for `GET /api/v2/effects/filtered`: the full current filter
 * state (including the edited group, which the server strips by index) plus an
 * `editing=<part>:<slot>` param. Cost-parse errors are ignored so narrowing is
 * never blocked by an in-progress cost edit.
 */
export function buildFilteredEffectsQuery(
  state: FilterState,
  target: FilteredEffectsTarget,
): URLSearchParams {
  const params = new URLSearchParams();
  appendFilterParams(params, state);

  const slot =
    target.region === 'echo'
      ? 'support'
      : String(compactedSlotIndex(state, target.slotIndex));
  params.set('editing', `${EFFECT_PART_NAMES[target.part]}:${slot}`);

  return params;
}

export function buildFilteredEffectsUrl(
  state: FilterState,
  target: FilteredEffectsTarget,
): string {
  const qs = buildFilteredEffectsQuery(state, target).toString();
  const path = `/api/v2/effects/filtered${qs ? `?${qs}` : ''}`;
  const apiBase = getApiBaseUrl();
  return apiBase ? `${apiBase}${path}` : path;
}

export function getApiBaseUrl(): string {
  const base = import.meta.env.VITE_API_BASE_URL?.trim() ?? '';
  return base.replace(/\/$/, '');
}

export function buildFullUrl(
  state: FilterState,
  options?: BuildQueryOptions,
): BuildQueryResult & {
  url?: string;
  queryString?: string;
} {
  const result = buildRequestPath(state, options);
  if (!result.ok || !result.path) {
    return result;
  }
  const apiBase = getApiBaseUrl();
  const url = apiBase ? `${apiBase}${result.path}` : result.path;
  return { ...result, url };
}
