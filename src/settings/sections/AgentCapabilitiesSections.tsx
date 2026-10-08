import { SectionHeader } from "@/settings/components/SectionHeader";
import {
  ResourceFeedback,
  ResourceList,
  ResourceRefresh,
  useAgentResources,
} from "@/settings/components/AgentResources";
import { useTranslation } from "@/modules/i18n";
export { MemorySection } from "@/settings/sections/MemorySection";

export function SkillsSection() {
  const tr = useTranslation();
  const resources = useAgentResources();
  return (
    <div className="flex flex-col gap-7">
      <div className="flex items-start justify-between gap-3">
        <SectionHeader title={tr("Skills")} />
        <ResourceRefresh resources={resources} />
      </div>
      <ResourceFeedback resources={resources} />
      {resources.data && (
        <ResourceList
          entries={resources.data.skills}
          empty="No matching resources."
        />
      )}
    </div>
  );
}

export function HooksSection() {
  const tr = useTranslation();
  const resources = useAgentResources();
  return (
    <div className="flex flex-col gap-7">
      <div className="flex items-start justify-between gap-3">
        <SectionHeader title={tr("Hooks")} />
        <ResourceRefresh resources={resources} />
      </div>
      <p className="rounded-lg bg-muted/40 px-4 py-3 text-ui-sm text-muted-foreground">
        {tr(
          "The current agent runtime does not execute lifecycle hooks. Declarations below are informational and remain inactive.",
        )}
      </p>
      <ResourceFeedback resources={resources} />
      {resources.data && (
        <ResourceList
          entries={resources.data.hooks}
          empty="No lifecycle hooks declared by enabled plugins."
          inactive
        />
      )}
    </div>
  );
}
