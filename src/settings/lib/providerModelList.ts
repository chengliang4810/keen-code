const contextNumberFormat = new Intl.NumberFormat("en-US", {
  notation: "compact",
  maximumFractionDigits: 1,
});

export function formatModelContextWindow(value: number): string {
  return Number.isFinite(value) ? contextNumberFormat.format(value) : "";
}

export function reorderProviderModels<T extends { id: string }>(
  models: readonly T[],
  activeId: string,
  overId: string,
): readonly T[] {
  const from = models.findIndex((model) => model.id === activeId);
  const to = models.findIndex((model) => model.id === overId);
  if (from < 0 || to < 0 || from === to) return models;
  const next = [...models];
  const [model] = next.splice(from, 1);
  next.splice(to, 0, model);
  return next;
}
