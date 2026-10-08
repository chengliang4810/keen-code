// Modified for RCode. See NOTICE.
import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  ArrowReloadHorizontalIcon,
  Globe02Icon,
  LinkSquare02Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";

type PortPreset = {
  port: number;
  label: string;
};

// Curated dev-server ports. Ordered by frontend frequency, then backend.
const PORT_PRESETS: readonly PortPreset[] = [
  { port: 5173, label: "Vite" },
  { port: 5174, label: "Vite (alt)" },
  { port: 3000, label: "Next.js" },
  { port: 3001, label: "Next.js (alt)" },
  { port: 4173, label: "Vite preview" },
  { port: 4200, label: "Angular" },
  { port: 4321, label: "Astro" },
  { port: 5500, label: "Live Server" },
  { port: 6006, label: "Storybook" },
  { port: 8080, label: "Webpack" },
  { port: 8081, label: "Metro" },
  { port: 8000, label: "Django / FastAPI" },
  { port: 8888, label: "Jupyter" },
  { port: 5000, label: "Flask" },
  { port: 7860, label: "Gradio" },
  { port: 11434, label: "Ollama" },
];

export type PreviewAddressBarHandle = {
  focus: () => void;
};

type Props = {
  url: string;
  onSubmit: (url: string) => void;
  onReload: () => void;
};

export const PreviewAddressBar = forwardRef<PreviewAddressBarHandle, Props>(
  function PreviewAddressBar({ url, onSubmit, onReload }, ref) {
    const tr = useTranslation();
    const [draft, setDraft] = useState(url);
    const inputRef = useRef<HTMLInputElement>(null);

    // Keep draft in sync when the parent updates the URL externally
    // (AI tool, detected localhost chip, etc.).
    useEffect(() => {
      setDraft(url);
    }, [url]);

    useImperativeHandle(
      ref,
      () => ({
        focus: () => {
          const el = inputRef.current;
          if (!el) return;
          el.focus();
          el.select();
        },
      }),
      [],
    );

    const [notice, setNotice] = useState<string | null>(null);
    const [checkingPort, setCheckingPort] = useState<number | null>(null);

    const submit = () => {
      const next = normalizeUrl(draft);
      if (!next) {
        setNotice("Enter a URL or pick a port preset.");
        return;
      }
      setNotice(null);
      if (next !== url) onSubmit(next);
      else onReload();
    };

    const tryPort = async (port: number) => {
      setNotice(null);
      setCheckingPort(port);
      const url = `http://localhost:${port}`;
      const ok = await probeUrl(url);
      setCheckingPort(null);
      if (!ok) {
        setNotice(`No server listening on :${port}.`);
        return;
      }
      setDraft(url);
      onSubmit(url);
    };

    return (
      <div className="shrink-0 border-b border-border/60">
        <div className="flex h-9 items-center gap-1 bg-card/40 px-1.5">
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={onReload}
            title={tr("Reload")}
            className="size-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <HugeiconsIcon
              icon={ArrowReloadHorizontalIcon}
              size={14}
              strokeWidth={1.75}
            />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                title={tr("Common dev-server ports")}
                className="h-7 shrink-0 gap-1 rounded-md px-1.5 text-ui-base text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <HugeiconsIcon
                  icon={Globe02Icon}
                  size={13}
                  strokeWidth={1.75}
                />
                <span className="hidden sm:inline">{tr("Ports")}</span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent
              align="start"
              className="max-h-80 min-w-56 overflow-y-auto"
            >
              {PORT_PRESETS.map((p) => (
                <DropdownMenuItem
                  key={p.port}
                  onSelect={(e) => {
                    e.preventDefault();
                    void tryPort(p.port);
                  }}
                >
                  <span className="flex-1">{tr(p.label)}</span>
                  <span className="text-ui-sm text-muted-foreground">
                    {checkingPort === p.port ? tr("checking…") : `:${p.port}`}
                  </span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <div className="flex min-w-0 flex-1 items-center">
            <Input
              ref={inputRef}
              value={draft}
              placeholder={tr("http://localhost:3000")}
              spellCheck={false}
              autoComplete="off"
              className="h-7 w-full bg-muted/60 px-2 text-ui-base placeholder:text-muted-foreground/70 focus-visible:ring-0"
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submit();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  setDraft(url);
                  inputRef.current?.blur();
                }
              }}
            />
          </div>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={() => {
              if (url) void openUrl(url).catch(console.error);
            }}
            title={tr("Open in system browser")}
            className="size-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            disabled={!url}
          >
            <HugeiconsIcon
              icon={LinkSquare02Icon}
              size={14}
              strokeWidth={1.75}
            />
          </Button>
        </div>
        {notice ? (
          <div className="flex items-center gap-1.5 bg-amber-500/8 px-3 py-1 text-ui-sm text-amber-600 dark:text-amber-400">
            <span className="truncate">{notice}</span>
            <button
              type="button"
              onClick={() => setNotice(null)}
              className="ml-auto rounded px-1 text-ui-base opacity-80 hover:bg-accent hover:opacity-100"
            >
              {tr("Dismiss")}
            </button>
          </div>
        ) : null}
      </div>
    );
  },
);

async function probeUrl(url: string): Promise<boolean> {
  try {
    await fetch(url, {
      method: "GET",
      mode: "no-cors",
      cache: "no-store",
      signal: AbortSignal.timeout(900),
    });
    return true;
  } catch {
    return false;
  }
}

function normalizeUrl(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  if (/^localhost(:|\/|$)/i.test(trimmed)) return `http://${trimmed}`;
  if (/^\d{1,3}(\.\d{1,3}){3}(:|\/|$)/.test(trimmed))
    return `http://${trimmed}`;
  if (/^[\w.-]+\.[a-z]{2,}/i.test(trimmed)) return `https://${trimmed}`;
  return trimmed;
}
