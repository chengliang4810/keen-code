/** 子 Agent 模板元数据（职责描述与模型标签）的按需拉取。 */

import { useEffect, useState } from "react";
import * as api from "@/lib/api";

/** `useSubagentMetadata` 的输入依赖。 */
export interface UseSubagentMetadataOptions {
  /** 当前项目根；模板列表按项目作用域读取。 */
  projectPath: string | null;
  /** 展示中子代理的稳定身份键；变化时重新拉取，空串不拉取。 */
  subagentIdentityKey: string;
}

/** `useSubagentMetadata` 返回的元数据映射。 */
export interface SubagentMetadataState {
  /** 子代理名称 → 职责描述。 */
  descriptions: Record<string, string>;
  /** 子代理名称 → 展示用模型标签（已剥掉 provider 前缀）。 */
  modelLabels: Record<string, string>;
}

/** 随展示中的子代理身份拉取本地子代理模板元数据。 */
export function useSubagentMetadata(
  options: UseSubagentMetadataOptions,
): SubagentMetadataState {
  const { projectPath, subagentIdentityKey } = options;
  const [descriptions, setSubagentDescriptions] = useState<
    Record<string, string>
  >({});
  const [modelLabels, setSubagentModelLabels] = useState<Record<string, string>>(
    {},
  );
  useEffect(() => {
    if (!api.isTauri() || !subagentIdentityKey) return;
    let cancelled = false;
    void api
      .agentsList(projectPath)
      .then(({ agents }) => {
        if (cancelled) return;
        setSubagentDescriptions(
          Object.fromEntries(
            agents.map((agent) => [agent.name, agent.description.trim()]),
          ),
        );
        setSubagentModelLabels(
          Object.fromEntries(
            agents.flatMap((agent) => {
              if (!agent.model) return [];
              const model = agent.model.split("::").at(-1)?.trim();
              return model ? [[agent.name, model]] : [];
            }),
          ),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [projectPath, subagentIdentityKey]);

  return { descriptions, modelLabels };
}
