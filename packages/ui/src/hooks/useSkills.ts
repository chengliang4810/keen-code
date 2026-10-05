import { useEffect, useMemo, useRef, useState } from "react";
import type { ZCodeSkillReferenceCatalogEntry } from "@zcode/shared";
import { useWorkspaceServicesResolution } from "@/hooks/useWorkspaceServices.js";
import { logger } from "@/logger.js";

interface ConversationSkillCatalogState {
  skills: ZCodeSkillReferenceCatalogEntry[];
  authority: "session" | "workspace" | null;
  loading: boolean;
  error: string | null;
}

interface ScopedConversationSkillCatalogState {
  scope: object | null;
  value: ConversationSkillCatalogState;
}

const EMPTY_STATE: ConversationSkillCatalogState = {
  skills: [],
  authority: null,
  loading: false,
  error: null,
};

const EMPTY_SCOPED_STATE: ScopedConversationSkillCatalogState = {
  scope: null,
  value: EMPTY_STATE,
};

interface UseSkillsOptions {
  workspacePath: string;
  workspaceIdentity?: string;
  sessionId: string | null;
  enabled: boolean;
  preferredRemoteSessionId?: string;
}

/**
 * 构造 Skill catalog 请求的 authority 边界。
 * 草稿传 null 时必须省略 sessionId，交给 Rust 扫描当前工作区；只有已有会话
 * 才携带 sessionId 读取其冻结目录。这里集中构造参数，避免把 undefined/null
 * 意外变成会话请求并遮住草稿阶段刚写入的 Skill。
 */
export function buildSkillReferenceCatalogParams(
  options: Pick<
    UseSkillsOptions,
    "workspacePath" | "workspaceIdentity" | "sessionId"
  >,
  remoteSessionId?: string,
) {
  return {
    workspacePath: options.workspacePath,
    ...(options.workspaceIdentity ? { workspaceIdentity: options.workspaceIdentity } : {}),
    ...(remoteSessionId ? { remoteSessionId } : {}),
    ...(options.sessionId ? { sessionId: options.sessionId } : {}),
  };
}

/**
 * Composer 的 Skill catalog。
 * 草稿以 workspace 当前扫描为 authority；已有 Session 以对应 AgentRuntime 冻结快照为
 * authority。prewarm 只服务首发与附件 transaction，不改变草稿的 catalog authority。
 * workspace/session/remote attachment/runtime 代次变化时，旧异步结果一律不得回填。
 */
export function useSkills(options: UseSkillsOptions): ConversationSkillCatalogState {
  const resolution = useWorkspaceServicesResolution(
    options.workspacePath,
    options.preferredRemoteSessionId,
    options.workspaceIdentity,
  );
  const [scopedState, setScopedState] =
    useState<ScopedConversationSkillCatalogState>(EMPTY_SCOPED_STATE);
  const [runtimeRevision, setRuntimeRevision] = useState(0);
  const requestSeqRef = useRef(0);
  const workspaceKey = options.workspaceIdentity?.trim() || options.workspacePath;
  const remoteSessionId =
    resolution.remoteSessionId ?? options.preferredRemoteSessionId ?? undefined;
  const services = resolution.services;
  const rpcReady = resolution.rpcReady;
  const requestKey = `${workspaceKey}|${remoteSessionId ?? "local"}|${options.sessionId ?? "draft"}|runtime:${runtimeRevision}`;

  useEffect(() => {
    if (!options.enabled || !options.sessionId || !rpcReady) return;
    const subscription = services.zcodeAgentService.onAgentRuntimeRestarted((event) => {
      if (event.workspaceKey !== workspaceKey) return;
      // runtime 重建后 workspace/session key 不变，旧 catalog 会继续命中。
      // 显式推进代次，使冷恢复后的新 runtime 必须重新提供一次 Session authority。
      setRuntimeRevision((current) => current + 1);
    });
    return () => subscription.dispose();
  }, [options.enabled, options.sessionId, rpcReady, services, workspaceKey]);

  // 用 scope 身份隔离渲染：key 切换后的 effect 尚未执行时也只返回空态，避免旧 Session
  // 或旧 remote attachment 的 Skill 在一帧内泄漏到新 Composer。
  const requestScope = useMemo(
    () => ({}),
    [options.enabled, remoteSessionId, requestKey, rpcReady, services],
  );

  useEffect(() => {
    if (!options.enabled || !options.workspacePath || !rpcReady) return;
    const seq = ++requestSeqRef.current;
    let cancelled = false;
    setScopedState({
      scope: requestScope,
      value: { skills: [], authority: null, loading: true, error: null },
    });
    const params = buildSkillReferenceCatalogParams(options, remoteSessionId);
    services.zcodeAgentService
      .getSkillReferenceCatalog(params)
      .then((result) => {
        if (cancelled || seq !== requestSeqRef.current) return;
        setScopedState({
          scope: requestScope,
          value: {
            skills: result.skills,
            authority: result.authority,
            loading: false,
            error: null,
          },
        });
      })
      .catch((error: unknown) => {
        if (cancelled || seq !== requestSeqRef.current) return;
        const message = error instanceof Error ? error.message : String(error);
        logger.warn("[useSkills] 拉取对话 Skill catalog 失败", {
          error: message,
          requestKey,
        });
        setScopedState({
          scope: requestScope,
          value: { skills: [], authority: null, loading: false, error: message },
        });
      });
    return () => {
      cancelled = true;
    };
  }, [
    options.enabled,
    options.sessionId,
    options.workspaceIdentity,
    options.workspacePath,
    remoteSessionId,
    requestKey,
    requestScope,
    rpcReady,
    services,
  ]);

  if (
    !options.enabled ||
    !options.workspacePath ||
    !rpcReady ||
    scopedState.scope !== requestScope
  ) {
    return EMPTY_STATE;
  }
  return scopedState.value;
}
