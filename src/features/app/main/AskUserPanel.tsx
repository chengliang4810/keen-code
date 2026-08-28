import type {
  Dispatch,
  RefObject,
  SetStateAction,
} from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { AskUserPayload } from "@/lib/session";
import { AskUserModal } from "@/components/AskUserModal";
import { sessionResolveAskUser } from "@/lib/acp/api";
import { toElicitationAnswers } from "@/lib/elicitation";
import { localizeUiError } from "@/lib/session";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

export interface AskUserPanelProps {
  askUser: AskUserPayload | null;
  askUserWrapRef: RefObject<HTMLDivElement | null>;
  locale: Locale;
  tr: Translator;
  clearPendingAskUser: (sessionId?: string | null, rpcId?: number) => void;
  setAskUser: SetState<AskUserPayload | null>;
  showToast: (message: string, duration?: number) => void;
}

export function AskUserPanel({
  askUser,
  askUserWrapRef,
  locale,
  tr,
  clearPendingAskUser,
  setAskUser,
  showToast,
}: AskUserPanelProps) {
  if (!askUser) return null;

  return (
    <div ref={askUserWrapRef} className="ask-user-wrap">
      <AskUserModal
        payload={askUser}
        labels={{
          title: tr("askUser.title"),
          submit: tr("askUser.submit"),
          next: tr("askUser.next"),
          cancel: tr("askUser.cancel"),
          otherPlaceholder: tr("askUser.otherPlaceholder"),
          freeTextHint: tr("askUser.freeTextHint"),
          multiHint: tr("askUser.multiHint"),
          close: tr("common.close"),
        }}
        onSubmit={async (answers) => {
          const payload = askUser;
          try {
            await sessionResolveAskUser({
              decision: "accepted",
              answers: toElicitationAnswers(payload, answers),
              rpcId: payload.rpcId,
            });
            clearPendingAskUser(payload.sessionId, payload.rpcId);
            setAskUser((current) =>
              current?.rpcId === payload.rpcId ? null : current,
            );
          } catch (error) {
            showToast(localizeUiError(error, locale), 4500);
          }
        }}
        onCancel={async () => {
          const payload = askUser;
          try {
            await sessionResolveAskUser({
              decision: "cancelled",
              rpcId: payload.rpcId,
            });
          } catch {
            // 取消后仍关闭当前卡片。
          }
          clearPendingAskUser(payload.sessionId, payload.rpcId);
          setAskUser((current) =>
            current?.rpcId === payload.rpcId ? null : current,
          );
        }}
      />
    </div>
  );
}
