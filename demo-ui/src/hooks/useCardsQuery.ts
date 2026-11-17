import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { buildFullUrl } from '../api/buildQuery';
import type { ApiError, CardV2, CardsIter, FamilyMatchV2, FilterState } from '../types';

const DEBOUNCE_MS = 300;

export type QueryStatus = 'idle' | 'loading' | 'success' | 'error';

export type CardsQueryState = {
  status: QueryStatus;
  cards: CardV2[];
  families: FamilyMatchV2[] | null;
  iter: CardsIter | null;
  error: string | null;
  durationMs: number | null;
  fetchedAt: number | null;
  url: string | null;
  queryString: string | null;
  handCostError: string | null;
  reserveCostError: string | null;
  skipped: boolean;
  loadingMore: boolean;
  lastPageCount: number;
  hasMore: boolean;
  loadMore: () => void;
};

const initialState: Omit<CardsQueryState, 'loadMore' | 'hasMore'> = {
  status: 'idle',
  cards: [],
  families: null,
  iter: null,
  error: null,
  durationMs: null,
  fetchedAt: null,
  url: null,
  queryString: null,
  handCostError: null,
  reserveCostError: null,
  skipped: false,
  loadingMore: false,
  lastPageCount: 0,
};

function serializeFilters(state: FilterState): string {
  return JSON.stringify(state);
}

async function fetchCardsPage(
  url: string,
  signal: AbortSignal,
): Promise<{
  cards: CardV2[];
  families: FamilyMatchV2[] | null;
  iter: CardsIter;
  durationMs: number;
}> {
  const start = performance.now();
  const response = await fetch(url, { signal });
  const durationMs = performance.now() - start;

  if (!response.ok) {
    let message = `HTTP ${response.status}`;
    try {
      const body = (await response.json()) as ApiError;
      if (body.error) {
        message = body.error;
      }
    } catch {
      // ignore JSON parse errors
    }
    throw new Error(message);
  }

  const data = (await response.json()) as {
    cards: CardV2[];
    families?: FamilyMatchV2[];
    iter: CardsIter;
  };
  return {
    cards: data.cards,
    families: data.families ?? null,
    iter: data.iter,
    durationMs,
  };
}

export function useCardsQuery(filters: FilterState): CardsQueryState {
  const [queryState, setQueryState] =
    useState<Omit<CardsQueryState, 'loadMore' | 'hasMore'>>(initialState);
  const serialized = useMemo(() => serializeFilters(filters), [filters]);
  const filtersRef = useRef(filters);
  filtersRef.current = filters;

  const abortRef = useRef<AbortController | null>(null);
  const loadMoreAbortRef = useRef<AbortController | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const iterRef = useRef<CardsIter | null>(null);
  iterRef.current = queryState.iter;

  const loadingMoreRef = useRef(false);
  loadingMoreRef.current = queryState.loadingMore;

  const statusRef = useRef<QueryStatus>('idle');
  statusRef.current = queryState.status;

  const skippedRef = useRef(false);
  skippedRef.current = queryState.skipped;

  useEffect(() => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }

    debounceRef.current = setTimeout(() => {
      const parsed = buildFullUrl(filters);

      if (!parsed.ok) {
        abortRef.current?.abort();
        loadMoreAbortRef.current?.abort();
        setQueryState({
          ...initialState,
          status: 'idle',
          handCostError: parsed.handCostError ?? null,
          reserveCostError: parsed.reserveCostError ?? null,
          url: null,
          queryString: null,
          skipped: true,
        });
        return;
      }

      abortRef.current?.abort();
      loadMoreAbortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;

      const url = parsed.url!;
      const queryString = parsed.queryString ?? '';

      setQueryState({
        ...initialState,
        status: 'loading',
        url,
        queryString,
        skipped: false,
      });

      void (async () => {
        try {
          const { cards, families, iter, durationMs } = await fetchCardsPage(
            url,
            controller.signal,
          );

          if (controller.signal.aborted) {
            return;
          }

          setQueryState({
            status: 'success',
            cards,
            families,
            iter,
            error: null,
            durationMs,
            fetchedAt: Date.now(),
            url,
            queryString,
            handCostError: null,
            reserveCostError: null,
            skipped: false,
            loadingMore: false,
            lastPageCount: cards.length,
          });
        } catch (err) {
          if (controller.signal.aborted) {
            return;
          }
          const message =
            err instanceof Error ? err.message : 'Request failed';
          setQueryState({
            ...initialState,
            status: 'error',
            error: message,
            fetchedAt: Date.now(),
            url,
            queryString,
            skipped: false,
          });
        }
      })();
    }, DEBOUNCE_MS);

    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [serialized, filters]);

  const loadMore = useCallback(() => {
    if (filtersRef.current.withFamilies) {
      return;
    }

    const iter = iterRef.current;
    if (
      iter?.cursor === undefined ||
      loadingMoreRef.current ||
      statusRef.current === 'loading' ||
      skippedRef.current
    ) {
      return;
    }

    const parsed = buildFullUrl(filtersRef.current, { cursor: iter.cursor });
    if (!parsed.ok || !parsed.url) {
      return;
    }

    loadMoreAbortRef.current?.abort();
    const controller = new AbortController();
    loadMoreAbortRef.current = controller;

    setQueryState((prev) => ({
      ...prev,
      loadingMore: true,
      error: null,
    }));

    void (async () => {
      try {
        const { cards: pageCards, families: pageFamilies, iter: nextIter, durationMs } =
          await fetchCardsPage(parsed.url!, controller.signal);

        if (controller.signal.aborted) {
          return;
        }

        setQueryState((prev) => ({
          ...prev,
          status: 'success',
          cards: [...prev.cards, ...pageCards],
          families: pageFamilies ?? prev.families,
          iter: nextIter,
          error: null,
          durationMs,
          fetchedAt: Date.now(),
          loadingMore: false,
          lastPageCount: pageCards.length,
        }));
      } catch (err) {
        if (controller.signal.aborted) {
          return;
        }
        const message =
          err instanceof Error ? err.message : 'Request failed';
        setQueryState((prev) => ({
          ...prev,
          status: prev.cards.length > 0 ? 'success' : 'error',
          error: message,
          loadingMore: false,
          fetchedAt: Date.now(),
        }));
      }
    })();
  }, []);

  useEffect(() => {
    return () => {
      abortRef.current?.abort();
      loadMoreAbortRef.current?.abort();
    };
  }, []);

  const hasMore = !filters.withFamilies && queryState.iter?.cursor !== undefined;

  return {
    ...queryState,
    hasMore,
    loadMore,
  };
}
