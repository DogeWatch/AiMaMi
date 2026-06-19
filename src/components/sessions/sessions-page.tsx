import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, Folder, MessageSquareText, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { BentoCard } from "@/components/ui/bento-card";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Skeleton } from "@/components/ui/skeleton";
import { PageHeader } from "@/components/ui/page-header";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { SessionProviderBucketPayload, SessionRecordPayload } from "@/types";

const SESSIONS_QUERY_KEY = ["codex-sessions"] as const;

interface SessionGroup {
  key: string;
  name: string;
  path: string;
  sessions: SessionRecordPayload[];
}

export function SessionsPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [migrationLedgerPath, setMigrationLedgerPath] = useState<string | null>(null);
  const [migrationResult, setMigrationResult] = useState<string | null>(null);

  const sessionsQuery = useQuery({
    queryKey: SESSIONS_QUERY_KEY,
    queryFn: () => api.loadSessions(),
  });

  const archiveMutation = useMutation({
    mutationFn: (ids: string[]) => api.deleteSessions(ids),
    onSuccess: () => {
      setSelected(new Set());
      void queryClient.invalidateQueries({ queryKey: SESSIONS_QUERY_KEY });
    },
  });

  const prepareMigrationMutation = useMutation({
    mutationFn: () => api.prepareSessionProviderMigration(),
    onSuccess: (result) => {
      setMigrationLedgerPath(result.data.path);
      setMigrationResult(null);
    },
  });

  const migrateProviderBucketsMutation = useMutation({
    mutationFn: () => api.migrateSessionProviderBucketsToActive(),
    onSuccess: (result) => {
      setMigrationLedgerPath(null);
      setMigrationResult(
        t("sessions.migrationApplied", { count: result.data.length }),
      );
      void queryClient.invalidateQueries({ queryKey: SESSIONS_QUERY_KEY });
    },
  });

  const payload = sessionsQuery.data?.data;
  const sessions = payload?.items ?? [];
  const groups = useMemo(() => buildSessionGroups(sessions), [sessions]);
  const selectedIds = [...selected];

  const toggleSelected = (id: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const archiveSelected = () => {
    if (selectedIds.length === 0 || archiveMutation.isPending) return;
    archiveMutation.mutate(selectedIds);
  };

  const migrateProviderBuckets = () => {
    if (migrateProviderBucketsMutation.isPending) return;
    if (!window.confirm(t("sessions.migrationConfirm"))) return;
    migrateProviderBucketsMutation.mutate();
  };

  return (
    <div className="space-y-5">
      <PageHeader
        title={t("nav.sessions")}
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => sessionsQuery.refetch()}
              disabled={sessionsQuery.isFetching}
            >
              <RefreshCw
                className={cn("h-4 w-4", sessionsQuery.isFetching && "animate-spin")}
              />
              <span>{t("mcp.refresh")}</span>
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={archiveSelected}
              disabled={selectedIds.length === 0 || archiveMutation.isPending}
            >
              <Archive className="h-4 w-4" />
              <span>{t("sessions.archiveSelected", { count: selectedIds.length })}</span>
            </Button>
          </>
        }
      />

      <div className="grid gap-4 md:grid-cols-3">
        <SessionStatCard
          label={t("sessions.total")}
          value={String(payload?.total ?? sessions.length)}
        />
        <SessionStatCard
          label={t("sessions.activeProvider")}
          value={payload?.activeModelProvider ?? "openai"}
        />
        <SessionStatCard
          label={t("sessions.providerBuckets")}
          value={formatProviderBuckets(payload?.providerBuckets ?? [])}
          compact
        />
      </div>

      <div className="text-xs text-muted-foreground">
        {payload?.stateIndexAvailable
          ? t("sessions.stateProviderBuckets", {
              buckets: formatProviderBuckets(payload.stateProviderBuckets),
            })
          : t("sessions.stateIndexUnavailable", {
              error: payload?.stateIndexError ?? "-",
            })}
      </div>
      <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
        {payload?.migrationPreview.required ? (
          <span>
            {t("sessions.migrationPreview", {
              source: payload.migrationPreview.sourceModelProvider,
              target: payload.migrationPreview.targetModelProvider,
              files: payload.migrationPreview.fileSessionCount,
              state: payload.migrationPreview.stateThreadCount ?? "-",
            })}
          </span>
        ) : (
          <span>
            {t("sessions.migrationAligned", {
              target: payload?.activeModelProvider ?? "openai",
            })}
          </span>
        )}
        {payload?.migrationPreview.required ? (
          <Button
            variant="outline"
            size="sm"
            onClick={() => prepareMigrationMutation.mutate()}
            disabled={prepareMigrationMutation.isPending || migrateProviderBucketsMutation.isPending}
          >
            <span>
              {prepareMigrationMutation.isPending
                ? t("sessions.preparingMigration")
                : t("sessions.prepareMigration")}
            </span>
          </Button>
        ) : null}
        <Button
          variant="outline"
          size="sm"
          onClick={migrateProviderBuckets}
          disabled={migrateProviderBucketsMutation.isPending}
        >
          <span>
            {migrateProviderBucketsMutation.isPending
              ? t("sessions.migratingProviderBuckets")
              : t("sessions.migrateProviderBuckets")}
          </span>
        </Button>
      </div>
      {migrationLedgerPath ? (
        <div className="text-xs text-muted-foreground">
          {t("sessions.migrationLedgerCreated", { path: migrationLedgerPath })}
        </div>
      ) : null}
      {migrationResult ? (
        <div className="text-xs text-muted-foreground">{migrationResult}</div>
      ) : null}
      {prepareMigrationMutation.isError || migrateProviderBucketsMutation.isError ? (
        <div className="text-xs text-destructive">
          {String(prepareMigrationMutation.error ?? migrateProviderBucketsMutation.error)}
        </div>
      ) : null}

      {sessionsQuery.isLoading ? (
        <BentoCard>
          <Skeleton className="h-5 w-40" />
          <Skeleton className="mt-4 h-16 w-full" />
          <Skeleton className="mt-3 h-16 w-full" />
        </BentoCard>
      ) : sessionsQuery.isError ? (
        <BentoCard>
          <div className="text-sm font-medium text-destructive">
            {t("sessions.loadFailed")}
          </div>
          <div className="mt-2 text-xs text-muted-foreground">
            {String(sessionsQuery.error)}
          </div>
        </BentoCard>
      ) : groups.length === 0 ? (
        <BentoCard>
          <div className="flex items-center gap-3 text-sm text-muted-foreground">
            <MessageSquareText className="h-4 w-4" />
            <span>{t("sessions.empty")}</span>
          </div>
        </BentoCard>
      ) : (
        <div className="space-y-4">
          {groups.map((group) => (
            <BentoCard key={group.key}>
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <div className="flex items-center gap-2 text-sm font-semibold">
                    <Folder className="h-4 w-4 text-muted-foreground" />
                    <span className="truncate">{group.name}</span>
                  </div>
                  <div className="mt-1 truncate text-xs text-muted-foreground">
                    {group.path}
                  </div>
                </div>
                <div className="text-xs text-muted-foreground">
                  {t("sessions.count", { count: group.sessions.length })}
                </div>
              </div>

              <div className="mt-4 divide-y divide-border rounded-[8px] border border-border">
                {group.sessions.map((session) => (
                  <label
                    key={session.id}
                    className="flex cursor-pointer items-center gap-3 px-4 py-3 hover:bg-muted/45"
                  >
                    <Checkbox
                      checked={selected.has(session.id)}
                      onCheckedChange={() => toggleSelected(session.id)}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">
                        {session.threadName || session.id}
                      </div>
                      <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                        <span>{session.id}</span>
                        {session.modelProvider ? <span>{session.modelProvider}</span> : null}
                        <span>{formatSessionTime(session.updatedAt)}</span>
                        <span>{formatBytes(session.fileSize)}</span>
                      </div>
                    </div>
                  </label>
                ))}
              </div>
            </BentoCard>
          ))}
        </div>
      )}

      {archiveMutation.isError ? (
        <div className="text-sm text-destructive">{String(archiveMutation.error)}</div>
      ) : null}
    </div>
  );
}

function SessionStatCard({
  label,
  value,
  compact = false,
}: {
  label: string;
  value: string;
  compact?: boolean;
}) {
  return (
    <BentoCard compact>
      <div className="text-xs font-medium uppercase text-muted-foreground">{label}</div>
      <div
        className={cn(
          "mt-3 font-semibold",
          compact ? "truncate text-sm" : "text-2xl",
        )}
        title={value}
      >
        {value}
      </div>
    </BentoCard>
  );
}

function buildSessionGroups(sessions: SessionRecordPayload[]): SessionGroup[] {
  const grouped = new Map<string, SessionRecordPayload[]>();
  for (const session of sessions) {
    const key = session.projectPath || "__ungrouped__";
    const bucket = grouped.get(key) ?? [];
    bucket.push(session);
    grouped.set(key, bucket);
  }

  return [...grouped.entries()]
    .map(([key, items]) => ({
      key,
      name: items[0]?.projectName || (key === "__ungrouped__" ? "Ungrouped" : lastPathSegment(key)),
      path: key === "__ungrouped__" ? "" : key,
      sessions: [...items].sort((left, right) => right.updatedAt - left.updatedAt),
    }))
    .sort((left, right) => latestUpdated(right.sessions) - latestUpdated(left.sessions));
}

function latestUpdated(sessions: SessionRecordPayload[]) {
  return Math.max(0, ...sessions.map((session) => session.updatedAt));
}

function lastPathSegment(value: string) {
  return value.split(/[\\/]/).filter(Boolean).pop() ?? value;
}

function formatSessionTime(value: number) {
  if (!value) return "-";
  const date = new Date(value > 10_000_000_000 ? value : value * 1000);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatBytes(bytes: number) {
  if (!bytes) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function formatProviderBuckets(buckets: SessionProviderBucketPayload[]) {
  if (buckets.length === 0) return "-";
  return buckets
    .map((bucket) => `${bucket.active ? "*" : ""}${bucket.modelProvider}:${bucket.count}`)
    .join("  ");
}
