import { useState } from 'react'
import { endpoints } from '@/api/endpoints'
import { usePublicSettings } from '@/api/queries'
import { formatBytes, formatDateTime } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { useNotifications } from '@/state/notifications'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconBell } from '@/components/layout/icons'
import { ConfirmModal, Modal } from '@/components/ui/Modal'
import { Badge, Copyable, RoleBadge } from '@/components/ui/display'
import { Button, Card, CardHeader, Checkbox, Input, cx } from '@/components/ui/primitives'

const THEME_OPTIONS = [
  { value: 'system', label: '跟随系统' },
  { value: 'dark', label: '深色' },
  { value: 'light', label: '浅色' },
] as const

export function SettingsPage() {
  const { user, memberships, currentMembership, organizationId, logout } = useAuth()
  const { theme, setTheme } = useTheme()
  const settings = usePublicSettings()
  const toast = useToast()
  const notifications = useNotifications()
  const [confirming, setConfirming] = useState(false)
  const [confirmingAccountDeletion, setConfirmingAccountDeletion] = useState(false)
  const [confirmingOrgDeletion, setConfirmingOrgDeletion] = useState(false)
  const [organizationConfirmation, setOrganizationConfirmation] = useState('')
  const [busy, setBusy] = useState(false)
  const [accountDeletionBusy, setAccountDeletionBusy] = useState(false)
  const [organizationDeletionBusy, setOrganizationDeletionBusy] = useState(false)

  const revokeAll = async () => {
    setBusy(true)
    try {
      await endpoints.revokeAllSessions()
      toast.info('已撤销所有会话', '需要重新登录')
      await logout()
    } catch (error) {
      toast.fromError(error, '撤销会话失败')
    } finally {
      setBusy(false)
      setConfirming(false)
    }
  }

  const exportOrganization = async () => {
    if (!organizationId) return
    try {
      const data = await endpoints.exportOrganization(organizationId)
      const url = URL.createObjectURL(
        new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' }),
      )
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = `nagisalake-${organizationId}-export.json`
      anchor.click()
      URL.revokeObjectURL(url)
      toast.success('组织数据导出已下载')
    } catch (error) {
      toast.fromError(error, '组织数据导出失败')
    }
  }

  const deleteAccount = async () => {
    setAccountDeletionBusy(true)
    try {
      await endpoints.deleteAccount()
      toast.info('账户已删除')
      await logout()
    } catch (error) {
      toast.fromError(error, '账户删除失败')
    } finally {
      setAccountDeletionBusy(false)
      setConfirmingAccountDeletion(false)
    }
  }

  const deleteOrganization = async () => {
    if (!organizationId) return
    setOrganizationDeletionBusy(true)
    try {
      await endpoints.deleteOrganization(organizationId, organizationConfirmation)
      toast.info('组织及其数据已删除')
      await logout()
    } catch (error) {
      toast.fromError(error, '组织删除失败')
    } finally {
      setOrganizationDeletionBusy(false)
      setConfirmingOrgDeletion(false)
      setOrganizationConfirmation('')
    }
  }

  return (
    <Page>
      <PageHeader title="设置" description="账户、外观与会话安全。" />

      {/* ---- Account-level settings ---- */}
      <section className="space-y-4">
        <Card>
          <CardHeader title="账户" />
          <dl className="space-y-3 p-5 text-xs">
            <div className="space-y-1">
              <dt className="text-muted">邮箱</dt>
              <dd className="flex flex-wrap items-center gap-2">
                {user?.email ?? '—'}
                {user &&
                  (user.email_verified ? (
                    <Badge tone="success">已验证</Badge>
                  ) : (
                    <Badge tone="warning">未验证</Badge>
                  ))}
              </dd>
            </div>
            <div className="space-y-1">
              <dt className="text-muted">用户 ID</dt>
              <dd>{user ? <Copyable value={user.id} /> : '—'}</dd>
            </div>
            <div className="space-y-1">
              <dt className="text-muted">状态</dt>
              <dd>{user?.status ?? '—'}</dd>
            </div>
            <div className="space-y-1">
              <dt className="text-muted">注册时间</dt>
              <dd className="font-mono">{formatDateTime(user?.created_at)}</dd>
            </div>
          </dl>
        </Card>

        <Card>
          <CardHeader title="外观" description="主题选择保存在本地，不影响其他设备。" />
          <div className="p-5">
            <div
              role="radiogroup"
              aria-label="主题"
              className="grid grid-cols-3 gap-1 rounded-lg border border-border bg-surface-2 p-1"
            >
              {THEME_OPTIONS.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  role="radio"
                  aria-checked={theme === option.value}
                  onClick={() => setTheme(option.value)}
                  className={cx(
                    'rounded-md px-3 py-1.5 text-xs transition',
                    theme === option.value
                      ? 'bg-accent text-accent-fg font-medium'
                      : 'text-muted hover:text-text',
                  )}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
        </Card>

        <Card>
          <CardHeader
            title={
              <span className="flex items-center gap-2">
                <IconBell className="size-4 text-accent" />
                作业完成提醒
              </span>
            }
            description="仅在作业从运行态进入终态时提醒；偏好保存在当前浏览器。"
          />
          <div className="space-y-4 p-5">
            <div className="space-y-1.5">
              <Checkbox
                label="浏览器系统通知"
                checked={notifications.desktopEnabled}
                disabled={notifications.permission === 'unsupported'}
                onChange={(enabled) => {
                  if (!enabled) {
                    notifications.disableDesktop()
                    return
                  }
                  void notifications.enableDesktop().then((permission) => {
                    if (permission === 'granted') toast.success('系统通知已开启')
                    else if (permission === 'denied') {
                      toast.error('通知权限被拒绝', '请在浏览器的网站设置中重新允许通知')
                    } else if (permission === 'unsupported') {
                      toast.info('当前环境不支持系统通知', '请使用 HTTPS 或 localhost 访问控制台')
                    }
                  })
                }}
              />
              <p className="pl-6 text-[11px] leading-relaxed text-subtle">
                {notifications.permission === 'unsupported'
                  ? '当前 origin 不支持 Notification API；应用内提醒仍然可用。'
                  : notifications.permission === 'denied'
                    ? '浏览器已阻止通知，需要从地址栏的网站权限中手动开启。'
                    : notifications.desktopEnabled
                      ? '切到其他标签页后，完成、失败和取消事件会触发系统通知。'
                      : '首次开启时浏览器会请求通知权限。'}
              </p>
            </div>

            <div className="border-t border-border/60 pt-4">
              <Checkbox
                label="播放完成提示音"
                checked={notifications.soundEnabled}
                onChange={notifications.setSoundEnabled}
              />
              <p className="mt-1.5 pl-6 text-[11px] leading-relaxed text-subtle">
                开启时会立即试听；成功与失败使用不同的双音提示。
              </p>
            </div>
          </div>
        </Card>

        <Card>
          <CardHeader title="会话安全" />
          <div className="space-y-4 p-5">
            <div className="space-y-2 text-xs leading-relaxed text-muted">
              <p>
                access token 只保存在页面内存中，刷新页面时通过 HttpOnly refresh cookie 恢复会话。
                每次 refresh 都会原子轮换 token，旧 token 重放会被拒绝。
              </p>
              {settings.data && (
                <p>
                  单文件上传上限 {formatBytes(settings.data.max_artifact_bytes)}，单次 PUT 直传对象
                  存储，不经过 Hub。
                </p>
              )}
            </div>
          </div>
        </Card>

        {/* Dangerous actions grouped into one card. */}
        <Card className="border-danger/30">
          <CardHeader
            title={
              <span className="flex items-center gap-2 text-danger">
                危险操作
              </span>
            }
            description="以下操作不可恢复，执行前请确认。"
          />
          <ul className="divide-y divide-danger/15">
            <li className="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
              <div className="min-w-0 space-y-1">
                <p className="text-xs font-medium text-text">撤销所有会话</p>
                <p className="text-[11px] leading-relaxed text-muted">
                  所有浏览器会话都会立即失效，包括当前这个。程序使用的 nsk_ API Key 和设备使用的 nwk_
                  凭据不受影响。
                </p>
              </div>
              <Button variant="danger" size="sm" onClick={() => setConfirming(true)}>
                撤销所有会话
              </Button>
            </li>

            <li className="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
              <div className="min-w-0 space-y-1">
                <p className="text-xs font-medium text-text">删除账户</p>
                <p className="text-[11px] leading-relaxed text-muted">
                  账户、OAuth 绑定、会话和你拥有的 Worker 设备会被删除。仍是任何组织 owner
                  时，服务端会先拒绝此操作。
                </p>
              </div>
              <Button
                variant="danger"
                size="sm"
                onClick={() => setConfirmingAccountDeletion(true)}
              >
                删除账户
              </Button>
            </li>

            {currentMembership?.role === 'owner' && (
              <li className="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
                <div className="min-w-0 space-y-1">
                  <p className="text-xs font-medium text-text">删除当前组织</p>
                  <p className="text-[11px] leading-relaxed text-muted">
                    会删除组织成员关系、作业、artifact 元数据、设备、凭据和对象存储文件，无法撤销。
                  </p>
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" variant="secondary" onClick={() => void exportOrganization()}>
                    导出组织数据
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    onClick={() => setConfirmingOrgDeletion(true)}
                  >
                    删除当前组织
                  </Button>
                </div>
              </li>
            )}
          </ul>
        </Card>
      </section>

      {/* ---- Organization-level settings (only when a membership is active) ---- */}
      {currentMembership && (
        <section className="mt-6 space-y-4">
          <Card>
            <CardHeader title="组织" description={`共 ${memberships.length} 个组织`} />
            <ul className="divide-y divide-border/60">
              {memberships.map((membership) => (
                <li
                  key={membership.organization_id}
                  className="flex items-center justify-between gap-3 px-5 py-3"
                >
                  <div className="min-w-0">
                    <p className="truncate text-xs font-medium">
                      {membership.organization_name}
                      {membership.organization_id === organizationId && (
                        <span className="ml-2 text-[10px] text-accent">当前</span>
                      )}
                    </p>
                    <Copyable
                      value={membership.organization_id}
                      display={`${membership.organization_id.slice(0, 16)}…`}
                      className="text-subtle"
                    />
                  </div>
                  <RoleBadge role={membership.role} />
                </li>
              ))}
            </ul>
            <div className="border-t border-border px-5 py-3">
              <p className="text-[11px] leading-relaxed text-subtle">
                组织切换只改变浏览器请求的 X-Organization-ID，服务端会重新校验 membership。API Key
                固定绑定创建时的组织，不受切换影响。
              </p>
            </div>
          </Card>
        </section>
      )}

      <ConfirmModal
        open={confirming}
        title="撤销所有会话"
        destructive
        confirmLabel="撤销并退出"
        loading={busy}
        description="所有浏览器会话都会立即失效，包括当前这个。程序使用的 nsk_ API Key 和设备使用的 nwk_ 凭据不受影响。"
        onConfirm={revokeAll}
        onClose={() => setConfirming(false)}
      />

      <ConfirmModal
        open={confirmingAccountDeletion}
        title="删除账户"
        destructive
        confirmLabel="永久删除"
        loading={accountDeletionBusy}
        description="账户、OAuth 绑定、会话和你拥有的 Worker 设备会被删除。仍是任何组织 owner 时，服务端会先拒绝此操作。"
        onConfirm={() => void deleteAccount()}
        onClose={() => setConfirmingAccountDeletion(false)}
      />

      <Modal
        open={confirmingOrgDeletion}
        title="删除组织"
        description="这会删除组织成员关系、作业、artifact 元数据、设备、凭据和对象存储文件，无法撤销。"
        onClose={() => {
          if (!organizationDeletionBusy) {
            setConfirmingOrgDeletion(false)
            setOrganizationConfirmation('')
          }
        }}
        footer={
          <>
            <Button
              size="sm"
              disabled={organizationDeletionBusy}
              onClick={() => setConfirmingOrgDeletion(false)}
            >
              取消
            </Button>
            <Button
              size="sm"
              variant="danger"
              loading={organizationDeletionBusy}
              disabled={organizationConfirmation !== organizationId}
              onClick={() => void deleteOrganization()}
            >
              永久删除组织
            </Button>
          </>
        }
      >
        <label className="block space-y-1.5 text-xs font-medium text-muted">
          <span>输入组织 ID 确认</span>
          <Input
            value={organizationConfirmation}
            onChange={(event) => setOrganizationConfirmation(event.target.value)}
            placeholder={organizationId ?? ''}
            autoFocus
          />
        </label>
        <p className="mt-2 break-all font-mono text-[11px] text-subtle">{organizationId}</p>
      </Modal>
    </Page>
  )
}
