import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  scope: string;
  compact?: boolean;
  resetKey?: string;
}

interface State {
  error: Error | null;
  details: string;
}

/**
 * Crash containment for route and feature surfaces. A malformed provider or
 * persisted record must never be able to replace the desktop workspace with
 * a silent black WebView.
 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, details: "" };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ details: info.componentStack ?? "" });
    console.error(`[${this.props.scope}] rendering failed`, error, info);
  }

  componentDidUpdate(previous: Props) {
    if (this.state.error && previous.resetKey !== this.props.resetKey) {
      this.setState({ error: null, details: "" });
    }
  }

  private reset = () => this.setState({ error: null, details: "" });

  render() {
    if (!this.state.error) return this.props.children;

    const message = this.state.error.message || "未知界面错误";
    return (
      <div
        className={
          "flex min-h-0 items-center justify-center bg-slate-50 p-4 dark:bg-slate-950 " +
          (this.props.compact ? "h-full" : "min-h-full")
        }
        role="alert"
      >
        <div className="card w-full max-w-2xl p-4">
          <div className="text-sm font-semibold text-red-700 dark:text-red-300">
            {this.props.scope}暂时无法显示
          </div>
          <p className="muted mt-2 text-sm">界面已安全隔离该异常，其他功能仍可继续使用。</p>
          <div className="mt-3 flex flex-wrap gap-2">
            <button className="btn-primary" onClick={this.reset}>
              重试此区域
            </button>
            <button className="btn" onClick={() => window.location.reload()}>
              重新加载应用
            </button>
          </div>
          {import.meta.env.DEV && (
            <details className="mt-3 rounded border border-slate-200 p-2 text-xs dark:border-slate-800">
              <summary className="cursor-pointer font-medium">开发诊断</summary>
              <pre className="num mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all text-red-700 dark:text-red-300">
                {message}
                {this.state.details}
              </pre>
            </details>
          )}
        </div>
      </div>
    );
  }
}
