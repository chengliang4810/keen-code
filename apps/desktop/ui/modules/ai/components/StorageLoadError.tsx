import { Button } from "@/components/ui/button";
import { useTranslation } from "@/modules/i18n";

export function StorageLoadError({
  error,
  loading,
  onRetry,
}: {
  error: string;
  loading: boolean;
  onRetry: () => void;
}) {
  const tr = useTranslation();
  return (
    <div
      role="alert"
      className="flex flex-col gap-2 px-2 text-ui-sm text-destructive"
    >
      <p className="break-words">{error}</p>
      <Button variant="ghost" size="sm" disabled={loading} onClick={onRetry}>
        {tr("Retry")}
      </Button>
    </div>
  );
}
