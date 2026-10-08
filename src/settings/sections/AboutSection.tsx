import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { useUpdater } from "@/modules/updater";
import { GithubIcon, Globe02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { getName, getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { arch, platform } from "@tauri-apps/plugin-os";
import { useEffect, useState } from "react";
import { SectionHeader } from "../components/SectionHeader";
import { PRODUCT } from "@/config/product";

const REPO_URL = PRODUCT.repository;
const WEBSITE = PRODUCT.website;

const PLATFORM_LABEL: Record<string, string> = {
  macos: "macOS",
  windows: "Windows",
  linux: "Linux",
  ios: "iOS",
  android: "Android",
  freebsd: "FreeBSD",
};

export function AboutSection() {
  const tr = useTranslation();
  const [version, setVersion] = useState("");
  const [name, setName] = useState("RCode");
  const [build, setBuild] = useState("");
  const { status, check, install } = useUpdater({ autoCheck: false });
  const checking = status.kind === "checking";
  const downloading = status.kind === "downloading";
  const available = status.kind === "available";
  const manualAvailable = status.kind === "manual-available";
  const ready = status.kind === "ready";
  const checkLabel = !PRODUCT.updatesConfigured
    ? tr("Updates not configured")
    : status.kind === "uptodate"
      ? "You're up to date"
      : status.kind === "error"
        ? "Check failed — retry"
        : checking
          ? "Checking…"
          : downloading
            ? "Downloading…"
            : ready
              ? "Restart to install"
              : available
                ? `Install v${status.update.version}`
                : manualAvailable
                  ? `Update to v${status.info.version}`
                  : "Check for updates";
  const onUpdateClick = () => {
    if (available) void install();
    else void check({ manual: true });
  };

  useEffect(() => {
    void getVersion().then(setVersion);
    void getName().then(setName);
    try {
      const p = platform();
      const a = arch();
      const platformLabel = PLATFORM_LABEL[p] ?? p;
      setBuild(`${platformLabel} · ${a}`);
    } catch {
      setBuild("");
    }
  }, []);

  return (
    <div className="flex flex-col gap-6">
      <SectionHeader title={tr("About")} description="" />

      <div className="flex items-center gap-4 rounded-xl border border-border/60 bg-card/60 p-5">
        <img src="/logo.png" alt="" className="size-12" draggable={false} />
        <div className="flex min-w-0 flex-col">
          <span className="text-ui-lg font-semibold tracking-tight">
            {name}
          </span>
          <span className="text-ui-caption text-muted-foreground">
            {tr("Rust and Result. An agent development workbench.")}
          </span>
          <span className="mt-1 font-mono text-ui-sm text-muted-foreground">
            {tr("v")}
            {version || "—"}
          </span>
        </div>
      </div>

      <dl className="grid grid-cols-[110px_1fr] gap-y-2.5 text-ui-base">
        <dt className="text-muted-foreground">{tr("Build")}</dt>
        <dd className="font-mono text-ui-base">
          {build
            ? tr("{value0} · v{value1}", { value0: build, value1: version })
            : tr("v{value0}", { value0: version })}
        </dd>

        <dt className="text-muted-foreground">{tr("Bundle ID")}</dt>
        <dd className="font-mono text-ui-base">{PRODUCT.bundleId}</dd>

        <dt className="text-muted-foreground">{tr("License")}</dt>
        <dd>{tr(PRODUCT.license)}</dd>

        <dt className="text-muted-foreground">{tr("Source code")}</dt>
        <dd>
          <button
            type="button"
            disabled={!REPO_URL}
            onClick={() => void openUrl(REPO_URL)}
            className="inline-flex items-center gap-1.5 rounded-md text-ui-base underline-offset-2 hover:text-foreground hover:underline"
          >
            <HugeiconsIcon icon={GithubIcon} size={12} strokeWidth={1.75} />
            {REPO_URL || tr("Not configured")}
          </button>
        </dd>
        <dt className="text-muted-foreground">{tr("Website")}</dt>
        <dd>
          <button
            type="button"
            disabled={!WEBSITE}
            onClick={() => void openUrl(WEBSITE)}
            className="inline-flex items-center gap-1.5 rounded-md text-ui-base underline-offset-2 hover:text-foreground hover:underline"
          >
            <HugeiconsIcon icon={Globe02Icon} size={12} strokeWidth={1.75} />
            {WEBSITE || tr("Not configured")}
          </button>
        </dd>
      </dl>

      <div className="flex flex-col gap-1.5">
        <div className="flex gap-2">
          <Button
            size="sm"
            onClick={onUpdateClick}
            disabled={
              !PRODUCT.updatesConfigured || checking || downloading || ready
            }
          >
            {checkLabel}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void openUrl(REPO_URL)}
            disabled={!REPO_URL}
            className="gap-1.5"
          >
            <HugeiconsIcon icon={GithubIcon} size={12} strokeWidth={1.75} />
            {tr("View on GitHub")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void openUrl(`${REPO_URL}/issues/new`)}
            disabled={!REPO_URL}
          >
            {tr("Report an issue")}
          </Button>
        </div>
        {status.kind === "error" && (
          <p className="font-mono text-ui-sm break-all text-destructive/80">
            {status.message}
          </p>
        )}
        {downloading && status.contentLength ? (
          <p className="text-ui-sm text-muted-foreground">
            {Math.min(
              100,
              Math.round((status.downloaded / status.contentLength) * 100),
            )}
            %
          </p>
        ) : null}
      </div>
    </div>
  );
}
