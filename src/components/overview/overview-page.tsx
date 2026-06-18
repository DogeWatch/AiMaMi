import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CheckCircle2,
  Folder,
  RefreshCw,
  Server,
  ShieldCheck,
  Sparkles,
  ToggleLeft,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { BentoCard } from "@/components/ui/bento-card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";

const RUNTIME_STATE_DISPLAY_QUERY_KEY = ["runtime-state", "display"] as const;

function formatTime(value?: number) {
  if (!value) return "-";
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(value));
}

function statusClass(ok: boolean) {
  return ok
    ? "border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-900/70 dark:bg-emerald-950/40 dark:text-emerald-300"
    : "border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-900/70 dark:bg-amber-950/40 dark:text-amber-300";
}

export function OverviewPage() {
  const { t } = useTranslation();

  const snapshotQuery = useQuery({
    queryKey: RUNTIME_STATE_DISPLAY_QUERY_KEY,
    queryFn: () => api.loadSnapshot(false),
  });

  const mcpQuery = useQuery({
    queryKey: ["mcp-servers", "overview"],
    queryFn: () => api.loadMcpServers(),
  });

  const skillsQuery = useQuery({
    queryKey: ["installed-skills", "overview"],
    queryFn: () => api.loadInstalledSkills(),
  });

  const status = snapshotQuery.data?.data.status;
  const paths = status?.paths;
  const mcpItems = mcpQuery.data?.data.items ?? [];
  const skillItems = skillsQuery.data?.data.items ?? [];
  const enabledMcpCount = mcpItems.filter((item) => item.enabled).length;

  const healthItems = [
    {
      label: t("overview.healthCodexHome"),
      ok: Boolean(paths?.codexHome),
      path: paths?.codexHome,
    },
    {
      label: t("overview.healthAuth"),
      ok: Boolean(paths?.authExists),
      path: paths?.authPath,
    },
    {
      label: t("overview.healthRegistry"),
      ok: Boolean(paths?.registryExists),
      path: paths?.registryPath,
    },
  ];

  const apiReachable = status?.apiConnectivity.usageStatus === "reachable";
  const apiUnreachable = status?.apiConnectivity.usageStatus === "unreachable";
  const isLoading = snapshotQuery.isLoading && !status;

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="grid gap-4 md:grid-cols-3">
          {Array.from({ length: 3 }).map((_, index) => (
            <BentoCard key={index} compact>
              <Skeleton className="h-4 w-24" />
              <Skeleton className="mt-4 h-8 w-20" />
              <Skeleton className="mt-3 h-3 w-32" />
            </BentoCard>
          ))}
        </div>
        <BentoCard>
          <Skeleton className="h-5 w-36" />
          <Skeleton className="mt-5 h-16 w-full" />
        </BentoCard>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="grid gap-4 md:grid-cols-3">
        <BentoCard compact>
          <StatHeader icon={ShieldCheck} label={t("overview.usageSource")} />
          <div className="mt-4 text-2xl font-semibold capitalize">
            {status?.usageSource ?? "-"}
          </div>
          <div className="mt-2 text-xs text-muted-foreground">
            {t("overview.lastUpdated", { time: formatTime(status?.lastScanAt) })}
          </div>
        </BentoCard>

        <BentoCard compact>
          <StatHeader icon={ToggleLeft} label={t("overview.autoSwitch")} />
          <div className="mt-4">
            <Badge
              variant="outline"
              className={statusClass(Boolean(status?.autoSwitch.enabled))}
            >
              {status?.autoSwitch.enabled ? t("overview.enabled") : t("overview.disabled")}
            </Badge>
          </div>
          <div className="mt-3 text-xs text-muted-foreground">
            {status?.autoSwitch.serviceLabel ?? "-"}
          </div>
        </BentoCard>

        <BentoCard compact>
          <StatHeader
            icon={apiUnreachable ? AlertCircle : CheckCircle2}
            label={apiUnreachable ? t("overview.apiUnreachable") : t("overview.apiReachable")}
          />
          <div className="mt-4">
            <Badge
              variant="outline"
              className={statusClass(apiReachable)}
            >
              {apiUnreachable ? t("overview.healthSyncError") : t("overview.healthOk")}
            </Badge>
          </div>
          <div className="mt-3 line-clamp-2 text-xs text-muted-foreground">
            {status?.apiConnectivity.usageLastError ?? t("overview.apiUnreachableHint")}
          </div>
        </BentoCard>
      </div>

      <div className="grid gap-4 lg:grid-cols-[1.2fr_0.8fr]">
        <BentoCard>
          <div className="flex items-start justify-between gap-3">
            <div>
              <h2 className="text-base font-semibold">{t("overview.healthTitle")}</h2>
              <p className="mt-1 text-sm text-muted-foreground">
                {t("overview.healthDetail")}
              </p>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => snapshotQuery.refetch()}
              disabled={snapshotQuery.isFetching}
            >
              <RefreshCw
                className={cn("h-4 w-4", snapshotQuery.isFetching && "animate-spin")}
              />
              <span>{t("mcp.refresh")}</span>
            </Button>
          </div>

          <div className="mt-5 space-y-3">
            {healthItems.map((item) => (
              <div
                key={item.label}
                className="flex items-center justify-between gap-4 rounded-lg border border-border px-4 py-3"
              >
                <div className="flex min-w-0 items-center gap-3">
                  <Folder className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <div className="min-w-0">
                    <div className="text-sm font-medium">{item.label}</div>
                    <div className="truncate text-xs text-muted-foreground">{item.path ?? "-"}</div>
                  </div>
                </div>
                <Badge variant="outline" className={statusClass(item.ok)}>
                  {item.ok ? t("overview.healthOk") : t("overview.healthMissing")}
                </Badge>
              </div>
            ))}
          </div>
        </BentoCard>

        <BentoCard>
          <h2 className="text-base font-semibold">{t("overview.title")}</h2>
          <div className="mt-5 space-y-4">
            <SummaryRow
              icon={Server}
              label={t("overview.statMcp")}
              value={String(mcpQuery.data?.data.total ?? mcpItems.length)}
              detail={t("overview.enabledCount", {
                enabled: enabledMcpCount,
                disabled: Math.max(mcpItems.length - enabledMcpCount, 0),
              })}
            />
            <SummaryRow
              icon={Sparkles}
              label={t("overview.statSkills")}
              value={String(skillsQuery.data?.data.total ?? skillItems.length)}
              detail={skillsQuery.data?.data.rootPath ?? "-"}
            />
          </div>
        </BentoCard>
      </div>
    </div>
  );
}

function StatHeader({
  icon: Icon,
  label,
}: {
  icon: typeof ShieldCheck;
  label: string;
}) {
  return (
    <div className="flex items-center gap-2 text-sm text-muted-foreground">
      <Icon className="h-4 w-4" />
      <span>{label}</span>
    </div>
  );
}

function SummaryRow({
  icon: Icon,
  label,
  value,
  detail,
}: {
  icon: typeof Server;
  label: string;
  value: string;
  detail: string;
}) {
  return (
    <div className="flex items-center justify-between gap-4 rounded-lg border border-border px-4 py-3">
      <div className="flex min-w-0 items-center gap-3">
        <Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0">
          <div className="text-sm font-medium">{label}</div>
          <div className="truncate text-xs text-muted-foreground">{detail}</div>
        </div>
      </div>
      <div className="text-xl font-semibold">{value}</div>
    </div>
  );
}
