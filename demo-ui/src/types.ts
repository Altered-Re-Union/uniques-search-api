export type EffectSlot = {
  t: string;
  c: string;
  o: string;
  /** How many main-effect lines (M1/M2/M3) must match; sent as `effect[N][matchCount]`. */
  matchCount: MatchCount;
};

export type MatchCount = 1 | 2 | 3;

export const EMPTY_EFFECT_SLOT: EffectSlot = {
  t: '',
  c: '',
  o: '',
  matchCount: 1,
};

/** One row from `GET /api/v2/effects`. */
export type EffectCatalogItem = {
  idGd: number;
  text: Record<string, string>;
  isEcho?: boolean;
  isMain?: boolean;
};

export type EffectsCatalogResponse = {
  triggers: EffectCatalogItem[];
  conditions: EffectCatalogItem[];
  output: EffectCatalogItem[];
};

export type EffectsCatalogStatus = 'loading' | 'ready' | 'error';

export type EffectMode = 'and' | 'or';

/** Which part of an effect group a combobox edits (maps to trigger/condition/output). */
export type EffectPart = 't' | 'c' | 'o';

/** Response from `GET /api/v2/effects/filtered`. */
export type EffectsFilteredResponse = {
  editing: string;
  idGds: number[];
};

export type FilterState = {
  effects: EffectSlot[];
  effectMode: EffectMode;
  support: EffectSlot;
  factions: string[];
  sets: string[];
  /** Unified search box: sent as `q` (OR across name substring / exact ref / exact collector number). */
  search: string;
  format: string;
  collection: string;
  handCost: string;
  reserveCost: string;
  limit: string;
  withFamilies: boolean;
  debugBgaTrigram: boolean;
};

export const FACTIONS = ['AX', 'BR', 'LY', 'MU', 'OR', 'YZ'] as const;

export type FactionCode = (typeof FACTIONS)[number];

/** Source set codes for merged ALL_SETS index (merge `--sets` order). */
export const SOURCE_SETS = [
  'COREKS',
  'CORE',
  'ALIZE',
  'BISE',
  'CYCLONE',
  'DUSTER',
  'EOLE',
] as const;

export type SourceSetCode = (typeof SOURCE_SETS)[number];

export const DEFAULT_FILTER_STATE: FilterState = {
  effects: [{ ...EMPTY_EFFECT_SLOT }],
  effectMode: 'and',
  support: { t: '', c: '', o: '', matchCount: 1 },
  factions: [],
  sets: [],
  search: '',
  format: '',
  collection: '',
  handCost: '',
  reserveCost: '',
  limit: '',
  withFamilies: false,
  debugBgaTrigram: false,
};

export type CardsIter = {
  total: number;
  cursor?: number;
};

export type CardFaction = {
  code: string;
  name: string;
};

export type CardSet = {
  reference: string;
  name: string;
  code?: string;
};

export type CardSubType = {
  reference: string;
  name: Record<string, string>;
};

export type CardV2 = {
  reference: string;
  name: Record<string, string>;
  artist: string;
  set: CardSet;
  cardSubTypes: CardSubType[];
  mainCost: number;
  recallCost: number;
  forestPower: number;
  mountainPower: number;
  oceanPower: number;
  faction: CardFaction;
  mainEffect: Record<string, string>;
  echoEffect: Record<string, string>;
  /** `SET-FAMILY-U-UID` (e.g. `BTG-011-U-5`); absent when the loaded index has no collector-number
   * data for that family. */
  collectorNumber?: string;
  debug_bga_trigram?: string;
};

export type FamilyMatchV2 = {
  familyId: string;
  count: number;
  reference: string;
  name: Record<string, string>;
};

export type CardsResponse = {
  iter: CardsIter;
  cards: CardV2[];
  families?: FamilyMatchV2[];
};

export type ApiError = {
  error: string;
};

export type CostParseResult =
  | { ok: true; values: number[] }
  | { ok: false; error: string };

export type BuildQueryResult =
  | { ok: true; params: URLSearchParams }
  | { ok: false; handCostError?: string; reserveCostError?: string };
