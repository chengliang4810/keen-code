type Props = {
  title: string;
  description?: string;
};

export function SectionHeader({ title, description }: Props) {
  return (
    <div className="flex flex-col gap-1">
      <h2 className="text-2xl font-semibold tracking-tight">{title}</h2>
      {description ? (
        <p className="text-ui-sm text-muted-foreground">{description}</p>
      ) : null}
    </div>
  );
}
