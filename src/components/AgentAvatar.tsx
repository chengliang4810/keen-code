import { Blobatar } from "@blobatar/react";
import { happy, sad } from "blobatar/expression";
import "blobatar/motion.css";
import {
  agentNicknameSeed,
  type AgentNicknameRef,
} from "@/lib/agentNicknames";

export function AgentAvatar({
  nickname,
  agentId,
  size,
  status,
  className,
}: {
  nickname: AgentNicknameRef | null;
  agentId: string;
  size: number;
  status?: "running" | "done" | "interrupted" | "failed";
  className?: string;
}) {
  const seed = nickname
    ? agentNicknameSeed(nickname)
    : `keencode-agent-id:${agentId}`;

  if (status === "running") {
    return (
      <Blobatar
        name={seed}
        size={size}
        animate="always"
        className={className}
        aria-hidden="true"
        focusable="false"
      />
    );
  }

  return (
    <Blobatar
      name={seed}
      size={size}
      expression={
        status === "done" ? happy : status === "failed" ? sad : undefined
      }
      className={className}
      alt=""
      draggable={false}
    />
  );
}
