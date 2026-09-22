import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Field, FieldDescription, FieldLabel } from "@appica/ui-react/field";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { IconWorld } from "@/components/icons";
import type { HostMode } from "./hostMode";
import "./host-mode.css";

export type WebLoginStatus = "signed-out" | "submitting" | "signed-in" | "error";

export interface WebLoginLabels {
  title: string;
  description: string;
  token: string;
  tokenPlaceholder: string;
  submit: string;
  submitting: string;
  signedIn: string;
  error: string;
  unsupportedHost: string;
}

export const defaultWebLoginLabels: WebLoginLabels = {
  title: "登录 KeenCode Web",
  description: "登录后可从浏览器访问已授权的远程会话。",
  token: "Web Token",
  tokenPlaceholder: "粘贴 Web Token",
  submit: "继续登录",
  submitting: "正在登录…",
  signedIn: "已登录",
  error: "登录失败，请检查 Token 或稍后重试。",
  unsupportedHost: "当前宿主不需要 Web 登录。",
};

export interface WebLoginPanelProps {
  hostMode: HostMode;
  status: WebLoginStatus;
  token: string;
  onTokenChange: (value: string) => void;
  onSubmit: () => void | Promise<void>;
  errorMessage?: string | null;
  labels?: Partial<WebLoginLabels>;
}

export function WebLoginPanel({
  hostMode,
  status,
  token,
  onTokenChange,
  onSubmit,
  errorMessage,
  labels: labelOverrides,
}: WebLoginPanelProps) {
  const labels = { ...defaultWebLoginLabels, ...labelOverrides };
  const isWeb = hostMode === "web" || hostMode === "mobile-remote";
  const submitting = status === "submitting";

  if (!isWeb) {
    return (
      <section
        className="host-panel host-panel--unsupported"
        data-host-mode={hostMode}
        data-testid="web-login-panel"
      >
        <div className="host-panel__icon" aria-hidden="true">
          <IconWorld size={20} />
        </div>
        <p>{labels.unsupportedHost}</p>
      </section>
    );
  }

  return (
    <section
      className="host-panel web-login-panel"
      data-host-mode={hostMode}
      data-status={status}
      data-testid="web-login-panel"
    >
      <div className="host-panel__heading">
        <div className="host-panel__icon" aria-hidden="true">
          <IconWorld size={20} />
        </div>
        <div>
          <h2>{labels.title}</h2>
          <p>{labels.description}</p>
        </div>
      </div>

      {status === "error" ? (
        <Alert variant="error" role="alert">
          <AlertDescription>{errorMessage || labels.error}</AlertDescription>
        </Alert>
      ) : null}
      {status === "signed-in" ? (
        <Alert variant="success" role="status">
          <AlertDescription>{labels.signedIn}</AlertDescription>
        </Alert>
      ) : null}

      <form
        className="web-login-panel__form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!submitting) void onSubmit();
        }}
      >
        <Field>
          <FieldLabel>{labels.token}</FieldLabel>
          <Input
            type="password"
            autoComplete="current-password"
            value={token}
            placeholder={labels.tokenPlaceholder}
            disabled={submitting || status === "signed-in"}
            onChange={(event) => onTokenChange(event.target.value)}
          />
          <FieldDescription>{labels.description}</FieldDescription>
        </Field>
        <div className="web-login-panel__actions">
          <Button type="submit" variant="primary" disabled={submitting || status === "signed-in"}>
            {submitting ? labels.submitting : labels.submit}
          </Button>
        </div>
      </form>
    </section>
  );
}
