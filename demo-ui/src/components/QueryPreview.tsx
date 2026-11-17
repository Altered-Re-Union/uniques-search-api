import { useState } from "react";

type QueryPreviewProps = {
  queryString: string | null;
  url: string | null;
};

/** Decode application/x-www-form-urlencoded query text for display. */
function decodeQueryText(encoded: string): string {
  try {
    return decodeURIComponent(encoded.replace(/\+/g, " "));
  } catch {
    return encoded;
  }
}

function buildDisplayPath(queryString: string | null): string {
  if (!queryString) {
    return "/api/v2/cards";
  }
  return `/api/v2/cards?${decodeQueryText(queryString)}`;
}

export function QueryPreview({ queryString, url }: QueryPreviewProps) {
  const [copied, setCopied] = useState(false);

  const displayPath = buildDisplayPath(queryString);

  const copyUrl = async () => {
    if (!url) {
      return;
    }
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  };

  return (
    <section className="shrink-0 rounded-lg border border-slate-700 bg-slate-900/60 p-4">
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="flex flex-col gap-2">
          <h2 className="text-xs font-semibold text-slate-200">
            Query preview:
          </h2>
          <pre className="font-mono text-xs text-sky-300/90">{displayPath}</pre>
        </div>
        <button
          type="button"
          onClick={() => void copyUrl()}
          disabled={!url}
          className="rounded border border-slate-600 px-2 py-1 text-xs text-slate-300 hover:bg-slate-800 disabled:opacity-40"
        >
          {copied ? "Copied" : "Copy URL"}
        </button>
      </div>
    </section>
  );
}
