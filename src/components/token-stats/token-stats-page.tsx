import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { PageHeader } from "@/components/ui/page-header";
import { api } from "@/lib/api";
import type { TokenStatsBucket } from "@/types";
import { cn } from "@/lib/utils";

function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "K";
  return String(n);
}

function StatCard({ label, bucket, loading }: { label: string; bucket?: TokenStatsBucket; loading: boolean }) {
  return (
    <div className="rounded-lg border border-border bg-card p-4 shadow-sm">
      <div className="mb-3 flex items-center justify-between">
        <h3 className="text-sm font-medium text-muted-foreground">{label}</h3>
      </div>
      {loading || !bucket ? (
        <div className="space-y-2">
          <Skeleton className="h-8 w-24" />
          <Skeleton className="h-4 w-32" />
        </div>
      ) : (
        <div className="space-y-1">
          <div className="flex items-baseline gap-2">
            <span className="text-2xl font-semibold tabular-nums">{formatTokens(bucket.totalTokens)}</span>
            <span className="text-xs text-muted-foreground">total tokens</span>
          </div>
          <div className="flex gap-4 text-xs text-muted-foreground">
            <span>In: {formatTokens(bucket.inputTokens)}</span>
            <span>Out: {formatTokens(bucket.outputTokens)}</span>
            <span>Reqs: {bucket.requestCount}</span>
          </div>
        </div>
      )}
    </div>
  );
}

function ModelTable({ bucket, loading }: { bucket?: TokenStatsBucket; loading: boolean }) {
  if (loading) {
    return <Skeleton className="h-32 w-full" />;
  }
  if (!bucket || bucket.models.length === 0) {
    return <div className="py-8 text-center text-sm text-muted-foreground">No data</div>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border text-left text-xs text-muted-foreground">
            <th className="pb-2 pr-4 font-medium">Model</th>
            <th className="pb-2 pr-4 text-right font-medium">Input</th>
            <th className="pb-2 pr-4 text-right font-medium">Output</th>
            <th className="pb-2 pr-4 text-right font-medium">Total</th>
            <th className="pb-2 text-right font-medium">Requests</th>
          </tr>
        </thead>
        <tbody>
          {bucket.models.map((m) => (
            <tr key={m.model} className="border-b border-border/50">
              <td className="py-2 pr-4 font-medium">{m.model}</td>
              <td className="py-2 pr-4 text-right tabular-nums text-muted-foreground">{formatTokens(m.inputTokens)}</td>
              <td className="py-2 pr-4 text-right tabular-nums text-muted-foreground">{formatTokens(m.outputTokens)}</td>
              <td className="py-2 pr-4 text-right tabular-nums font-medium">{formatTokens(m.totalTokens)}</td>
              <td className="py-2 text-right tabular-nums text-muted-foreground">{m.requestCount}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function TokenStatsPage() {
  const statsQuery = useQuery({
    queryKey: ["token-stats"],
    queryFn: () => api.loadTokenStats(),
    refetchInterval: 30_000,
  });

  const data = statsQuery.data?.data;
  const loading = statsQuery.isLoading;

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <PageHeader title="Token Usage" />
        <Button
          variant="ghost"
          size="sm"
          onClick={() => statsQuery.refetch()}
          disabled={statsQuery.isFetching}
        >
          <RefreshCw className={cn("size-4", statsQuery.isFetching && "animate-spin")} />
        </Button>
      </div>

      <div className="grid gap-4 sm:grid-cols-3">
        <StatCard label="Today" bucket={data?.today} loading={loading} />
        <StatCard label="Last 7 days" bucket={data?.sevenDays} loading={loading} />
        <StatCard label="Last 30 days" bucket={data?.thirtyDays} loading={loading} />
      </div>

      <div className="space-y-6">
        <div>
          <h3 className="mb-3 text-sm font-medium">Models - Today</h3>
          <ModelTable bucket={data?.today} loading={loading} />
        </div>
        <div>
          <h3 className="mb-3 text-sm font-medium">Models - Last 7 days</h3>
          <ModelTable bucket={data?.sevenDays} loading={loading} />
        </div>
        <div>
          <h3 className="mb-3 text-sm font-medium">Models - Last 30 days</h3>
          <ModelTable bucket={data?.thirtyDays} loading={loading} />
        </div>
      </div>
    </div>
  );
}
