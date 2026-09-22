import { IconBolt } from "@/components/icons";

interface StartupScreenProps {
  /** Windows 自绘标题栏需要为窗口控制按钮保留拖拽空隙。 */
  useCustomWindowChrome: boolean;
}

/** 只承担首帧品牌展示；本地数据恢复不得把用户阻塞在这里。 */
export function StartupScreen({
  useCustomWindowChrome,
}: StartupScreenProps) {
  return (
    <div
      className={
        "setup-gate" +
        (useCustomWindowChrome ? " setup-gate--custom-chrome" : "")
      }
      data-testid="setup-booting"
    >
      <div className="setup-gate__drag" data-tauri-drag-region />
      <div className="setup-gate__center">
        <div className="setup-hero">
          <div className="setup-logo">
            <IconBolt size={30} title="KeenCode" />
          </div>
          <h1 className="setup-title">KeenCode</h1>
          <p className="setup-subtitle">
            一款轻量、本地优先的桌面 AI 编码工具。
          </p>
        </div>
      </div>
    </div>
  );
}
