import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, PlugZap, RefreshCw, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import { BentoCard } from "@/components/ui/bento-card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { PageHeader } from "@/components/ui/page-header";
import { Skeleton } from "@/components/ui/skeleton";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { RelayProviderDraftPayload, RelayProviderPayload } from "@/types";

const RELAY_QUERY_KEY = ["relay-state"] as const;

interface ProviderFormState {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  wireApi: string;
}

interface ProviderPreset {
  id: string;
  name: string;
  baseUrl: string;
  model: string;
  wireApi?: string;
}

const DEFAULT_FORM: ProviderFormState = {
  id: "",
  name: "",
  baseUrl: "",
  apiKey: "",
  model: "",
  wireApi: "responses",
};

const PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "dashscope",
    name: "DashScope",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    model: "qwen3.7-max",
    wireApi: "responses",
  },
];

export function ProvidersPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [form, setForm] = useState<ProviderFormState>(DEFAULT_FORM);
  const [modelOptions, setModelOptions] = useState<string[]>([]);
  const [testMessage, setTestMessage] = useState<string>("");

  const relayQuery = useQuery({
    queryKey: RELAY_QUERY_KEY,
    queryFn: () => api.loadRelayState(),
  });

  const invalidateRelay = () => {
    void queryClient.invalidateQueries({ queryKey: RELAY_QUERY_KEY });
  };

  const saveMutation = useMutation({
    mutationFn: (input: RelayProviderDraftPayload) => api.upsertRelayProvider(input),
    onSuccess: () => {
      setForm(DEFAULT_FORM);
      setModelOptions([]);
      invalidateRelay();
    },
  });

  const activateMutation = useMutation({
    mutationFn: (providerId: string) => api.activateRelayProvider(providerId),
    onSuccess: invalidateRelay,
  });

  const deleteMutation = useMutation({
    mutationFn: (providerId: string) => api.deleteRelayProvider(providerId),
    onSuccess: invalidateRelay,
  });

  const testMutation = useMutation({
    mutationFn: (input: RelayProviderDraftPayload) => api.testRelayDraft(input),
    onSuccess: (result) => {
      const data = result.data;
      setModelOptions(data.models);
      setTestMessage(
        data.ok
          ? t("providers.testOk", { count: data.models.length })
          : data.errorMessage ?? t("providers.testFailed"),
      );
    },
    onError: (error) => {
      setModelOptions([]);
      setTestMessage(String(error));
    },
  });

  const state = relayQuery.data?.data;
  const providers = state?.providers ?? [];
  const formValid = Boolean(form.name.trim() && form.baseUrl.trim() && form.model.trim());
  const busy =
    saveMutation.isPending ||
    activateMutation.isPending ||
    deleteMutation.isPending ||
    testMutation.isPending;
  const sortedProviders = useMemo(
    () => [...providers].sort((left, right) => Number(right.active) - Number(left.active)),
    [providers],
  );

  const updateForm = (key: keyof ProviderFormState, value: string) => {
    setForm((current) => ({ ...current, [key]: value }));
  };

  const applyPreset = (preset: ProviderPreset) => {
    setForm({
      id: preset.id,
      name: preset.name,
      baseUrl: preset.baseUrl,
      apiKey: "",
      model: preset.model,
      wireApi: preset.wireApi ?? "responses",
    });
    setModelOptions([]);
    setTestMessage("");
  };

  const editProvider = (provider: RelayProviderPayload) => {
    setForm({
      id: provider.id,
      name: provider.name,
      baseUrl: provider.baseUrl,
      apiKey: "",
      model: provider.model,
      wireApi: provider.wireApi || "responses",
    });
    setModelOptions(provider.modelsSample ?? []);
    setTestMessage("");
  };

  const draftFromForm = (): RelayProviderDraftPayload => ({
    id: form.id.trim() || null,
    name: form.name.trim(),
    baseUrl: form.baseUrl.trim(),
    apiKey: form.apiKey.trim() || null,
    model: form.model.trim(),
    wireApi: form.wireApi.trim() || "responses",
  });

  const saveProvider = () => {
    if (!formValid || busy) return;
    saveMutation.mutate(draftFromForm());
  };

  const testProvider = () => {
    if (!form.baseUrl.trim() || testMutation.isPending) return;
    testMutation.mutate(draftFromForm());
  };

  return (
    <div className="space-y-5">
      <PageHeader
        title={t("nav.providers")}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => relayQuery.refetch()}
            disabled={relayQuery.isFetching}
          >
            <RefreshCw className={cn("h-4 w-4", relayQuery.isFetching && "animate-spin")} />
            <span>{t("mcp.refresh")}</span>
          </Button>
        }
      />

      <div className="grid gap-4 lg:grid-cols-[0.92fr_1.08fr]">
        <BentoCard>
          <div>
            <h2 className="text-base font-semibold">{t("providers.formTitle")}</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              {t("providers.formDescription")}
            </p>
          </div>

          <div className="mt-4 flex flex-wrap gap-2">
            {PROVIDER_PRESETS.map((preset) => (
              <Button
                key={preset.id}
                type="button"
                size="xs"
                variant="outline"
                onClick={() => applyPreset(preset)}
              >
                {preset.name}
              </Button>
            ))}
          </div>

          <div className="mt-5 grid gap-4">
            <Field label={t("providers.name")}>
              <Input value={form.name} onChange={(event) => updateForm("name", event.target.value)} />
            </Field>
            <Field label={t("providers.baseUrl")}>
              <Input value={form.baseUrl} onChange={(event) => updateForm("baseUrl", event.target.value)} />
            </Field>
            <Field label={t("providers.apiKey")}>
              <Input
                type="password"
                value={form.apiKey}
                placeholder={form.id ? t("providers.apiKeyPreserve") : ""}
                onChange={(event) => updateForm("apiKey", event.target.value)}
              />
            </Field>
            <Field label={t("providers.wireApi")}>
              <Input
                value={form.wireApi}
                readOnly
                aria-describedby="relay-wire-api-hint"
              />
              <p id="relay-wire-api-hint" className="text-xs text-muted-foreground">
                {t("providers.wireApiHint")}
              </p>
            </Field>
            <Field label={t("providers.model")}>
              <Input
                value={form.model}
                list="relay-model-options"
                onChange={(event) => updateForm("model", event.target.value)}
              />
              <datalist id="relay-model-options">
                {modelOptions.map((model) => (
                  <option key={model} value={model} />
                ))}
              </datalist>
            </Field>
          </div>

          <div className="mt-5 flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={testProvider}
              disabled={!form.baseUrl.trim() || testMutation.isPending}
            >
              <PlugZap className="h-4 w-4" />
              <span>{t("providers.test")}</span>
            </Button>
            <Button type="button" onClick={saveProvider} disabled={!formValid || busy}>
              {t("providers.save")}
            </Button>
          </div>

          {testMessage ? (
            <div className="mt-4 text-sm text-muted-foreground">{testMessage}</div>
          ) : null}
          <div className="mt-4 rounded-[8px] border border-border bg-muted/35 px-3 py-2 text-xs text-muted-foreground">
            {t("providers.workflowHint")}
          </div>
          {saveMutation.isError ? (
            <div className="mt-4 text-sm text-destructive">{String(saveMutation.error)}</div>
          ) : null}
        </BentoCard>

        <BentoCard>
          <div className="flex items-start justify-between gap-4">
            <div>
              <h2 className="text-base font-semibold">{t("providers.registryTitle")}</h2>
              <p className="mt-1 text-sm text-muted-foreground">
                {state?.sourcePath ?? "-"}
              </p>
            </div>
            <div className="max-w-[42%] truncate text-right text-xs text-muted-foreground" title={state?.codexConfigPath}>
              {state?.codexConfigPath ?? "-"}
            </div>
          </div>

          {state?.diagnostics ? (
            <div className="mt-4 grid gap-2 text-xs text-muted-foreground sm:grid-cols-2">
              <DiagnosticItem
                label={t("providers.diagnosticRegistry")}
                ok={state.diagnostics.registryExists}
              />
              <DiagnosticItem
                label={t("providers.diagnosticConfig")}
                ok={state.diagnostics.configExists}
              />
              <DiagnosticItem
                label={t("providers.diagnosticBlock")}
                ok={state.diagnostics.managedBlockPresent}
              />
              <DiagnosticItem
                label={t("providers.diagnosticActive")}
                ok={state.diagnostics.activeProviderConfigured}
              />
              <DiagnosticItem
                label={t("providers.diagnosticRelay")}
                ok={state.diagnostics.relayServerReachable}
              />
              {state.diagnostics.issueMessage ? (
                <div className="sm:col-span-2 text-destructive">
                  {state.diagnostics.issueMessage}
                </div>
              ) : null}
              {state.diagnostics.activeProviderConfigured ? (
                <div className="sm:col-span-2 rounded-[8px] border border-border bg-muted/35 px-3 py-2">
                  {t("providers.restartHint")}
                </div>
              ) : null}
            </div>
          ) : null}

          {relayQuery.isLoading ? (
            <div className="mt-5 space-y-3">
              <Skeleton className="h-16 w-full" />
              <Skeleton className="h-16 w-full" />
            </div>
          ) : sortedProviders.length === 0 ? (
            <div className="mt-5 rounded-[8px] border border-border px-4 py-6 text-sm text-muted-foreground">
              {t("providers.empty")}
            </div>
          ) : (
            <div className="mt-5 divide-y divide-border rounded-[8px] border border-border">
              {sortedProviders.map((provider) => {
                const configNeedsRepair =
                  provider.active && !state?.diagnostics.activeProviderConfigured;
                return (
                  <div key={provider.id} className="flex items-center gap-3 px-4 py-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <button
                          type="button"
                          className="truncate text-left text-sm font-medium hover:text-primary"
                          onClick={() => editProvider(provider)}
                        >
                          {provider.name}
                        </button>
                        {provider.active ? (
                          <CheckCircle2 className="h-4 w-4 text-emerald-500" />
                        ) : null}
                      </div>
                      <div className="mt-1 truncate text-xs text-muted-foreground">
                        {provider.model} · {provider.baseUrl}
                      </div>
                    </div>
                    <Button
                      type="button"
                      size="xs"
                      variant={provider.active && !configNeedsRepair ? "secondary" : "outline"}
                      onClick={() => activateMutation.mutate(provider.id)}
                      disabled={(provider.active && !configNeedsRepair) || busy}
                    >
                      {configNeedsRepair
                        ? t("providers.rewriteConfig")
                        : provider.active
                          ? t("providers.active")
                          : t("providers.activate")}
                    </Button>
                    <Button
                      type="button"
                      size="icon-sm"
                      variant="ghost"
                      onClick={() => deleteMutation.mutate(provider.id)}
                      disabled={busy}
                      aria-label={t("providers.delete")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                );
              })}
            </div>
          )}

          {activateMutation.isError || deleteMutation.isError ? (
            <div className="mt-4 text-sm text-destructive">
              {String(activateMutation.error ?? deleteMutation.error)}
            </div>
          ) : null}
        </BentoCard>
      </div>
    </div>
  );
}

function DiagnosticItem({ label, ok }: { label: string; ok: boolean }) {
  return (
    <div className="flex items-center gap-2">
      <span
        className={cn("h-2 w-2 rounded-full", ok ? "bg-emerald-500" : "bg-muted-foreground/40")}
      />
      <span>{label}</span>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="grid gap-2">
      <Label>{label}</Label>
      {children}
    </div>
  );
}
