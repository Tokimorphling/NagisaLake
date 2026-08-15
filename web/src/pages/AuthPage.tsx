import { useState } from 'react'
import { ApiError } from '@/api/client'
import { usePublicSettings } from '@/api/queries'
import { formatBytes } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { IconLogo } from '@/components/layout/icons'
import { Button, Field, Input } from '@/components/ui/primitives'

type Mode = 'login' | 'register'

const MIN_PASSWORD_LENGTH = 12

const OAUTH_LABELS = {
  google: 'Google',
  github: 'GitHub',
  linuxdo: 'Linux.do',
  oidc: '单点登录',
} as const

export function AuthPage() {
  const { login, register } = useAuth()
  const settings = usePublicSettings()
  const [mode, setMode] = useState<Mode>('login')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [organizationName, setOrganizationName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [showPassword, setShowPassword] = useState(false)

  const settingsLoading = settings.isLoading && !settings.data

  const registrationEnabled = settings.data?.registration_enabled ?? false
  const passwordAuthEnabled = settings.data?.password_auth_enabled ?? false
  const oauthProviders = settings.data?.oauth_providers ?? []
  const oauthError = new URLSearchParams(window.location.search).get('oauth_error')
  const passwordTooShort = mode === 'register' && password.length > 0 && password.length < MIN_PASSWORD_LENGTH

  const startOAuth = (provider: string) => {
    const redirect = encodeURIComponent('/')
    window.location.assign(`/api/v1/auth/oauth/${encodeURIComponent(provider)}/start?redirect=${redirect}`)
  }

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    setError(null)
    setBusy(true)
    try {
      if (mode === 'login') await login(email.trim(), password)
      else await register(email.trim(), password, organizationName)
    } catch (caught) {
      setError(
        caught instanceof ApiError ? caught.message : '请求失败，请确认 Hub 已启动并配置 PostgreSQL',
      )
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="aurora grid min-h-dvh place-items-center px-4 py-10">
      <div className="w-full max-w-md">
        <div className="mb-7 flex flex-col items-center gap-3 text-center">
          <IconLogo className="size-11" />
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Nagisalake</h1>
            <p className="mt-1 text-xs text-muted">连接云端 Hub 与 NAT 后的 ComfyUI 设备</p>
          </div>
        </div>

        <div className="rounded-xl border border-border bg-surface/90 p-5 shadow-[var(--shadow-card)] backdrop-blur">
          {settingsLoading && (
            <div className="space-y-3">
              <div className="skeleton h-10 rounded-lg" />
              <div className="skeleton h-10 rounded-lg" />
              <div className="skeleton h-10 rounded-lg" />
            </div>
          )}

          {!settingsLoading && oauthProviders.length > 0 && (
            <div className="space-y-3">
              {oauthProviders.map((provider) => (
                <Button
                  key={provider.name}
                  type="button"
                  variant="primary"
                  className="w-full"
                  onClick={() => startOAuth(provider.name)}
                >
                  使用 {OAUTH_LABELS[provider.kind]} 登录
                </Button>
              ))}
              {!registrationEnabled && (
                <p className="text-center text-[11px] leading-relaxed text-subtle">
                  当前已关闭新账户注册，已绑定账户仍可登录。
                </p>
              )}
            </div>
          )}

          {!settingsLoading && oauthError && (
            <p
              role="alert"
              className="mt-4 rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger"
            >
              OAuth 登录失败：{oauthError}
            </p>
          )}

          {!settingsLoading && passwordAuthEnabled && oauthProviders.length > 0 && (
            <div className="my-5 flex items-center gap-3 text-[10px] text-subtle">
              <span className="h-px flex-1 bg-border" />
              本地兼容登录
              <span className="h-px flex-1 bg-border" />
            </div>
          )}

          {!settingsLoading && passwordAuthEnabled && (
            <>
              <div className="mb-5 grid grid-cols-2 gap-1 rounded-lg border border-border bg-surface-2 p-1" role="tablist" aria-label="登录或注册">
                {(['login', 'register'] as const).map((value) => (
                  <button
                    key={value}
                    type="button"
                    role="tab"
                    aria-selected={mode === value}
                    onClick={() => {
                      setMode(value)
                      setError(null)
                    }}
                    disabled={value === 'register' && !registrationEnabled}
                    className={
                      mode === value
                        ? 'rounded-md bg-accent px-3 py-1.5 text-xs font-medium text-accent-fg'
                        : 'rounded-md px-3 py-1.5 text-xs text-muted transition hover:text-text disabled:cursor-not-allowed disabled:opacity-40'
                    }
                  >
                    {value === 'login' ? '登录' : '注册'}
                  </button>
                ))}
              </div>

              <form onSubmit={submit} className="space-y-4" aria-busy={busy}>
                <Field label="邮箱" required>
                  {(id) => (
                    <Input
                      id={id}
                      type="email"
                      required
                      autoComplete="email"
                      autoFocus={oauthProviders.length === 0}
                      placeholder="you@example.com"
                      value={email}
                      onChange={(event) => setEmail(event.target.value)}
                    />
                  )}
                </Field>

                <Field
                  label="密码"
                  required
                  error={passwordTooShort ? `密码至少需要 ${MIN_PASSWORD_LENGTH} 个字符` : null}
                  hint={mode === 'register' ? `至少 ${MIN_PASSWORD_LENGTH} 个字符` : undefined}
                >
                  {(id) => (
                    <div className="relative">
                      <Input
                        id={id}
                        type={showPassword ? 'text' : 'password'}
                        required
                        autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
                        placeholder="••••••••••••"
                        value={password}
                        onChange={(event) => setPassword(event.target.value)}
                      />
                      <button
                        type="button"
                        onClick={() => setShowPassword((prev) => !prev)}
                        className="absolute right-2 top-1/2 -translate-y-1/2 inline-flex size-7 items-center justify-center rounded-md text-subtle hover:bg-surface-2 hover:text-text transition"
                        aria-label={showPassword ? '隐藏密码' : '显示密码'}
                      >
                        {showPassword ? (
                          <svg viewBox="0 0 24 24" className="size-4" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                            <path d="M9.88 9.88a3 3 0 1 0 4.24 4.24" />
                            <path d="M10.73 5.08A10.43 10.43 0 0 1 12 5c7 0 10 7 10 7a13.16 13.16 0 0 1-1.67 2.68" />
                            <path d="M6.61 6.61A13.526 13.526 0 0 0 2 12s3 7 10 7a9.74 9.74 0 0 0 5.39-1.61" />
                            <line x1="2" y1="2" x2="22" y2="22" />
                          </svg>
                        ) : (
                          <svg viewBox="0 0 24 24" className="size-4" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                            <path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z" />
                            <circle cx="12" cy="12" r="3" />
                          </svg>
                        )}
                      </button>
                    </div>
                  )}
                </Field>

                {mode === 'register' && (
                  <Field label="组织名称" hint="留空则使用邮箱前缀自动命名">
                    {(id) => (
                      <Input
                        id={id}
                        maxLength={120}
                        placeholder="my workspace"
                        value={organizationName}
                        onChange={(event) => setOrganizationName(event.target.value)}
                      />
                    )}
                  </Field>
                )}

                {error && (
                  <p
                    role="alert"
                    className="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger"
                  >
                    {error}
                  </p>
                )}

                <Button
                  type="submit"
                  variant="primary"
                  className="w-full"
                  loading={busy}
                  disabled={!email.trim() || !password || passwordTooShort}
                >
                  {mode === 'login' ? '登录' : '创建账户'}
                </Button>
              </form>

              {mode === 'register' && !registrationEnabled && settings.isSuccess && !settingsLoading && (
                <p className="mt-4 rounded-lg border border-border bg-surface-2 px-3 py-2 text-xs leading-relaxed text-muted">
                  当前 Hub 已关闭注册。请联系管理员，或在 hub.toml 中设置
                  <code className="mx-1 font-mono text-[11px]">registration_enabled = true</code>。
                </p>
              )}
            </>
          )}

          {!settingsLoading && settings.isSuccess && !passwordAuthEnabled && oauthProviders.length === 0 && (
            <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs leading-relaxed text-warning">
              当前 Hub 没有可用的 OAuth provider，请联系管理员检查配置。
            </p>
          )}
        </div>

        {settings.data && (
          <p className="mt-5 text-center text-[11px] leading-relaxed text-subtle">
            单文件上传上限 {formatBytes(settings.data.max_artifact_bytes)} · 认证方式{' '}
            {settings.data.authentication.join(' / ')}
          </p>
        )}
        {settings.isError && (
          <p className="mt-5 text-center text-[11px] leading-relaxed text-danger">
            无法读取 Hub 公开设置，请确认 Hub 已在 127.0.0.1:9091 运行。
          </p>
        )}
      </div>
    </div>
  )
}
