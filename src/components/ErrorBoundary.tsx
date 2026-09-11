import * as React from "react";
import { reportFrontendError } from "@/lib/frontendDiagnostics";

/**
 * 渲染异常兜底边界。单个页面/面板崩溃时展示可恢复的错误卡片，
 * 避免 React 卸载整棵树导致整窗白屏。错误详情仅写入统一诊断日志。
 */
type ErrorBoundaryProps = {
  /** 崩溃影响范围的短描述，例如"会话时间线"。 */
  scope: string;
  /** 兜底卡片额外样式。 */
  className?: string;
  children: React.ReactNode;
};

type ErrorBoundaryState = { error: Error | null };

export class ErrorBoundary extends React.Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    reportFrontendError(
      "frontend.error_boundary",
      `scope=${this.props.scope} ${error instanceof Error ? error.stack || error.message : String(error)}\ncomponentStack=${errorInfo.componentStack ?? ""}`,
    );
  }

  private handleRetry = () => {
    this.setState({ error: null });
  };

  private handleReload = () => {
    window.location.reload();
  };

  render() {
    if (this.state.error) {
      return (
        <div
          role="alert"
          className={`error-boundary-fallback ${this.props.className ?? ""}`}
        >
          <p className="error-boundary-fallback__title">
            {this.props.scope}渲染失败，已阻止整窗崩溃。
          </p>
          <p className="error-boundary-fallback__desc">
            错误详情已写入诊断日志，重试或重载可恢复。
          </p>
          <div className="error-boundary-fallback__actions">
            <button type="button" className="btn btn--solid" onClick={this.handleRetry}>
              重试
            </button>
            <button type="button" className="btn btn--ghost" onClick={this.handleReload}>
              重载窗口
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
