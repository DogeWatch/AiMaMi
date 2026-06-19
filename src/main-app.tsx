import { Suspense, lazy, useCallback, useEffect, useState, type CSSProperties } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useTheme } from "@/hooks/use-theme";
import { useAccentColor } from "@/hooks/use-accent-color";
import { useAutoRefresh } from "@/hooks/use-auto-refresh";
import { useDeferredReady } from "@/hooks/use-deferred-ready";
import { useRouteTransition } from "@/hooks/use-route-transition";
import { PageStage } from "@/components/layout/page-stage";
import {
  AppSidebar,
  appNavItems,
  SIDEBAR_COLLAPSED_WIDTH_PX,
  SIDEBAR_EXPANDED_WIDTH_PX,
} from "@/components/layout/sidebar";
import { SiteHeader } from "@/components/site-header";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { Skeleton } from "@/components/ui/skeleton";
import { Toaster } from "@/components/ui/toaster";
import { createAppQueryClient } from "@/lib/query-client";
import { isMacPlatform } from "@/lib/platform";
import type { Route } from "@/types/navigation";
import "./lib/i18n";

const McpPage = lazy(() =>
  import("@/components/mcp/mcp-page").then((module) => ({ default: module.McpPage })),
);
const SkillsPage = lazy(() =>
  import("@/components/skills/skills-page").then((module) => ({ default: module.SkillsPage })),
);
const CustomInstructionsPage = lazy(() =>
  import("@/components/custom-instructions/custom-instructions-page").then((module) => ({ default: module.CustomInstructionsPage })),
);
const MaintenancePage = lazy(() =>
  import("@/components/maintenance/maintenance-page").then((module) => ({ default: module.MaintenancePage })),
);
const SettingsPage = lazy(() =>
  import("@/components/settings/settings-page").then((module) => ({ default: module.SettingsPage })),
);
const OverviewPage = lazy(() =>
  import("@/components/overview/overview-page").then((module) => ({ default: module.OverviewPage })),
);
const SessionsPage = lazy(() =>
  import("@/components/sessions/sessions-page").then((module) => ({ default: module.SessionsPage })),
);
const ProvidersPage = lazy(() =>
  import("@/components/providers/providers-page").then((module) => ({ default: module.ProvidersPage })),
);

const DRAG_REGION_HEIGHT = isMacPlatform() ? 48 : 0;
const queryClient = createAppQueryClient();

export function MainAppRoot() {
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider delayDuration={200}>
        <MainApp />
      </TooltipProvider>
    </QueryClientProvider>
  );
}

function MainApp() {
  const [route, setRoute] = useState<Route>("overview");
  const { theme, setTheme } = useTheme();
  const { accent, setAccent, heatmap, setHeatmap } = useAccentColor();
  const { i18n, t } = useTranslation();
  const [sidebarOpen, setSidebarOpen] = useState(
    () => localStorage.getItem("sidebar_collapsed") === "false",
  );
  const { refreshInterval, setRefreshInterval } = useAutoRefresh();
  const routeTransition = useRouteTransition(route, { durationMs: 240 });

  const handleThemeChange = useCallback((nextTheme: "light" | "dark" | "system") => {
    setTheme(nextTheme);
  }, [setTheme]);

  const prewarmRoutes = useDeferredReady(900);
  useEffect(() => {
    if (prewarmRoutes) {
      void Promise.allSettled([
        import("@/components/mcp/mcp-page"),
        import("@/components/skills/skills-page"),
        import("@/components/custom-instructions/custom-instructions-page"),
        import("@/components/maintenance/maintenance-page"),
        import("@/components/settings/settings-page"),
        import("@/components/overview/overview-page"),
        import("@/components/sessions/sessions-page"),
        import("@/components/providers/providers-page"),
      ]);
    }
  }, [prewarmRoutes]);

  const renderPage = (targetRoute: Route) => {
    switch (targetRoute) {
      case "overview":
        return <OverviewPage />;
      case "sessions":
        return <SessionsPage />;
      case "providers":
        return <ProvidersPage />;
      case "mcp":
        return <McpPage />;
      case "skills":
        return <SkillsPage />;
      case "customInstructions":
        return <CustomInstructionsPage />;
      case "maintenance":
        return <MaintenancePage />;
      case "settings":
        return (
          <SettingsPage
            theme={theme}
            onThemeChange={handleThemeChange}
            accent={accent}
            setAccent={setAccent}
            heatmap={heatmap}
            setHeatmap={setHeatmap}
            language={i18n.language}
            setLanguage={(lang) => {
              i18n.changeLanguage(lang);
              localStorage.setItem("app_language", lang);
            }}
            refreshInterval={refreshInterval}
            setRefreshInterval={setRefreshInterval}
          />
        );
      default:
        return null;
    }
  };

  const routeLabelKey = appNavItems.find((item) => item.route === route)?.labelKey ?? "nav.overview";

  const routeOrder: Route[] = [
    "overview",
    "sessions",
    "providers",
    "customInstructions",
    "mcp",
    "skills",
    "maintenance",
    "settings",
  ];

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#FFFFFF] dark:bg-background">
      <div
        className="fixed inset-x-0 top-0 z-[60]"
        data-tauri-drag-region
        style={{ WebkitAppRegion: "drag", height: DRAG_REGION_HEIGHT } as CSSProperties}
      />

      <SidebarProvider
        open={sidebarOpen}
        onOpenChange={(open) => {
          setSidebarOpen(open);
          localStorage.setItem("sidebar_collapsed", String(!open));
        }}
        style={
          {
            "--sidebar-width": `${SIDEBAR_EXPANDED_WIDTH_PX}px`,
            "--sidebar-width-icon": `${SIDEBAR_COLLAPSED_WIDTH_PX}px`,
          } as CSSProperties
        }
        className="flex min-h-0 flex-1 overflow-hidden"
      >
        <AppSidebar
          activeRoute={route}
          onNavigate={setRoute}
          onThemeChange={handleThemeChange}
        />
        <SidebarInset className="max-h-screen overflow-hidden">
          <SiteHeader title={t(routeLabelKey)} />
          <div className="relative min-h-0 flex-1 overflow-hidden">
            {routeOrder
              .filter((candidate) => routeTransition.mountedRoutes.includes(candidate))
              .map((candidate) => (
                <PageStage
                  key={candidate}
                  state={routeTransition.getStage(candidate)}
                >
                  <Suspense fallback={<PageShellSkeleton />}>
                    {renderPage(candidate)}
                  </Suspense>
                </PageStage>
              ))}
          </div>
        </SidebarInset>
      </SidebarProvider>

      <Toaster />
    </div>
  );
}

function PageShellSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-6 w-32" />
        <Skeleton className="h-4 w-72" />
      </div>
      <div className="rounded-2xl border border-border bg-card p-6">
        <div className="space-y-4">
          {Array.from({ length: 5 }).map((_, index) => (
            <div key={index} className="flex items-center justify-between border-b border-border/60 pb-4 last:border-b-0">
              <div className="space-y-2">
                <Skeleton className="h-4 w-36" />
                <Skeleton className="h-3 w-56" />
              </div>
              <Skeleton className="h-8 w-20 rounded-xl" />
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
