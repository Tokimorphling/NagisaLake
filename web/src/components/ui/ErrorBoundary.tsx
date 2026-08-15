import { Component, type ErrorInfo, type ReactNode } from 'react'
import { Button } from './primitives'

interface Props {
  children: ReactNode
}

interface State {
  error: Error | null
}

/**
 * Catches uncaught render exceptions so a malformed manifest, a null deref in
 * a chart, or any other render-time fault does not white-screen the entire
 * authenticated app with no recovery path.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('Unhandled render error:', error, info.componentStack)
  }

  handleReload = () => {
    this.setState({ error: null })
    window.location.reload()
  }

  handleGoHome = () => {
    this.setState({ error: null })
    window.location.href = '/'
  }

  render() {
    if (this.state.error) {
      return (
        <div className="grid min-h-dvh place-items-center p-4">
          <div className="flex max-w-md flex-col items-center gap-4 text-center">
            <div className="grid size-12 place-items-center rounded-xl border border-danger/30 bg-danger/10 text-danger">
              <svg viewBox="0 0 24 24" className="size-6" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M12 9v4m0 4h.01M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </div>
            <div className="space-y-1">
              <h1 className="text-lg font-semibold tracking-tight">页面渲染出错</h1>
              <p className="text-sm leading-relaxed text-muted">
                应用遇到了一个意外错误。可以刷新页面重试，或返回首页。
              </p>
            </div>
            <details className="w-full" {...(this.state.error.message ? {} : {})}>
              <summary className="cursor-pointer text-xs text-subtle">错误详情</summary>
              <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left text-xs text-subtle">
                {this.state.error.message || String(this.state.error)}
              </pre>
            </details>
            <div className="flex gap-2">
              <Button variant="secondary" size="sm" onClick={this.handleGoHome}>
                返回首页
              </Button>
              <Button variant="primary" size="sm" onClick={this.handleReload}>
                刷新页面
              </Button>
            </div>
          </div>
        </div>
      )
    }
    return this.props.children
  }
}
