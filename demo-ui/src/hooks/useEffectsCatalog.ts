import { useEffect, useState } from 'react';
import { getApiBaseUrl } from '../api/buildQuery';
import type {
  ApiError,
  EffectsCatalogResponse,
  EffectsCatalogStatus,
} from '../types';

export type EffectsCatalogState = {
  status: EffectsCatalogStatus;
  catalog: EffectsCatalogResponse | null;
  error: string | null;
};

const initialState: EffectsCatalogState = {
  status: 'loading',
  catalog: null,
  error: null,
};

export function useEffectsCatalog(): EffectsCatalogState {
  const [state, setState] = useState<EffectsCatalogState>(initialState);

  useEffect(() => {
    const controller = new AbortController();

    async function load() {
      const base = getApiBaseUrl();
      const url = `${base}/api/v2/effects?kind=unique`;

      try {
        const res = await fetch(url, { signal: controller.signal });
        if (!res.ok) {
          let message = `HTTP ${res.status}`;
          try {
            const body = (await res.json()) as ApiError;
            if (body.error) {
              message = body.error;
            }
          } catch {
            // ignore parse errors
          }
          setState({ status: 'error', catalog: null, error: message });
          return;
        }

        const catalog = (await res.json()) as EffectsCatalogResponse;
        setState({ status: 'ready', catalog, error: null });
      } catch (err) {
        if (controller.signal.aborted) {
          return;
        }
        const message =
          err instanceof Error ? err.message : 'Failed to load effects';
        setState({ status: 'error', catalog: null, error: message });
      }
    }

    void load();
    return () => controller.abort();
  }, []);

  return state;
}
