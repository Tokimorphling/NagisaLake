import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { endpoints } from '@/api/endpoints'
import { keys, useApiKeys, useWorkerCredentials } from '@/api/queries'
import { API_KEY_SCOPES } from '@/api/types'
import type { ApiKey, WorkerCredential } from '@/api/types'
import { formatDateTime } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import { IconKey, IconPlus } from '@/components/layout/icons'
import { ConfirmModal, Modal } from '@/components/ui/Modal'
import { SecretModal } from '@/components/ui/SecretModal'
import {
  Badge,
  EmptyState,
  ErrorState,
  RelativeTime,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, CardHeader, Field, Input, Select, cx } from '@/components/ui/primitives'

const EXPIRY_OPTIONS = [
  { value: '', label: '不过期' },
  { value: '86400', label: '1 天' },
  { value: '604800', label: '7 天' },
  { value: '2592000', label: '30 天' },
  { value: '31536000', label: '365 天' },
]

const DEFAULT_SCOPES = ['workflows:read', 'jobs:read', 'jobs:write', 'artifacts:write', 'artifacts:read']

type Tab = 'api-keys' | 'workers'

interface RevealedSecret {
  title: string
  description: string
  secret: string
}

export function CredentialsPage() {
  const { organizationId, atLeast } = useAuth()
  const [tab, setTab] = useState<Tab>('api-keys')
  const [revealed, setRevealed] = useState<RevealedSecret | null>(null)
  const apiKeys = useApiKeys(organizationId)
  const workerCredentials = useWorkerCredentials(organizationId)

  const tabCounts = {
    'api-keys': apiKeys.data?.length ?? 0,
    workers: workerCredentials.data?.length ?? 0,
  } as const

  return (
    <Page>
      <PageHeader
        title="凭据"
        description="程序调用使用 nsk_ API Key；边缘设备使用 nwk_ Worker 凭据。两者与浏览器 session 相互隔离，明文只显示一次。"
      />

      <div className="mb-4 inline-flex gap-1 rounded-lg border border-border bg-surface-2 p-1">
        {(
          [
            { value: 'api-keys', label: 'API Key' },
            { value: 'workers', label: 'Worker 凭据' },
          ] as const
        ).map((option) => (
          <button
            key={option.value}
            type="button"
            onClick={() => setTab(option.value)}
            className={cx(
              'rounded-md px-3 py-1.5 text-xs transition',
              tab === option.value
                ? 'bg-accent text-accent-fg font-medium'
                : 'text-muted hover:text-text',
            )}
          >
            {option.label}{' '}
            <span className={cx(tab === option.value ? 'opacity-80' : 'text-subtle')}>
              ({tabCounts[option.value]})
            </span>
          </button>
        ))}
      </div>

      {tab === 'api-keys' ? (
        <ApiKeysPanel
          organizationId={organizationId}
          canManage={atLeast('member')}
          onReveal={setRevealed}
        />
      ) : (
        <WorkerCredentialsPanel
          organizationId={organizationId}
          canManage={atLeast('member')}
          onReveal={setRevealed}
        />
      )}

      <SecretModal
        open={revealed !== null}
        title={revealed?.title ?? ''}
        description={revealed?.description ?? ''}
        secret={revealed?.secret ?? ''}
        onClose={() => setRevealed(null)}
      />
    </Page>
  )
}

/* ------------------------------------------------------------- API keys */

function ApiKeysPanel({
  organizationId,
  canManage,
  onReveal,
}: {
  organizationId: string | null
  canManage: boolean
  onReveal: (secret: RevealedSecret) => void
}) {
  const apiKeys = useApiKeys(organizationId)
  const queryClient = useQueryClient()
  const toast = useToast()
  const [creating, setCreating] = useState(false)
  const [revoking, setRevoking] = useState<ApiKey | null>(null)
  const [busy, setBusy] = useState(false)

  const revoke = async () => {
    if (!revoking || !organizationId) return
    setBusy(true)
    try {
      await endpoints.revokeApiKey(organizationId, revoking.id)
      void queryClient.invalidateQueries({ queryKey: keys.apiKeys(organizationId) })
      toast.success('API Key 已撤销', revoking.name)
      setRevoking(null)
    } catch (error) {
      toast.fromError(error, '撤销失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <Card>
        <CardHeader
          title="API Key"
          description="固定绑定当前组织，不能通过 X-Organization-ID 跨租户。scope 不会提升你的组织角色。"
          actions={
            <Button size="sm" variant="primary" disabled={!canManage} onClick={() => setCreating(true)}>
              <IconPlus className="size-3.5" />
              新建
            </Button>
          }
        />
        {apiKeys.isLoading ? (
          <SkeletonRows rows={3} />
        ) : apiKeys.isError ? (
          <ErrorState
            message={(apiKeys.error as Error).message}
            onRetry={() => void apiKeys.refetch()}
          />
        ) : (apiKeys.data?.length ?? 0) === 0 ? (
          <EmptyState
            icon={<IconKey className="size-8" />}
            title="还没有 API Key"
            description="SDK 与脚本使用 nsk_ Key 调用 /api/v1，不使用浏览器 session，也不参与 refresh。"
          />
        ) : (
          <Table>
            <thead>
              <tr>
                <Th>名称</Th>
                <Th>前缀</Th>
                <Th>Scope</Th>
                <Th>最后使用</Th>
                <Th>状态</Th>
                <Th className="text-right">操作</Th>
              </tr>
            </thead>
            <tbody>
              {apiKeys.data?.map((key) => {
                const status = credentialStatus(key.revoked_at, key.expires_at)
                return (
                  <tr key={key.id} className={cx('transition hover:bg-surface-2/50', status.dimmed && 'opacity-60')}>
                    <Td>
                      <p className="text-xs font-medium">{key.name}</p>
                      <p className="text-[10px] text-subtle">
                        创建于 {formatDateTime(key.created_at)}
                      </p>
                    </Td>
                    <Td>
                      <code className="font-mono text-[11px] text-muted">{key.prefix}…</code>
                    </Td>
                    <Td>
                      <div className="flex max-w-64 flex-wrap gap-1">
                        {key.scopes.slice(0, 3).map((scope) => (
                          <Badge key={scope} tone="info">
                            {scope}
                          </Badge>
                        ))}
                        {key.scopes.length > 3 && <Badge>+{key.scopes.length - 3}</Badge>}
                      </div>
                    </Td>
                    <Td className="text-xs whitespace-nowrap text-muted">
                      {key.last_used_at ? <RelativeTime value={key.last_used_at} /> : '从未使用'}
                    </Td>
                    <Td>
                      <Badge tone={status.tone}>{status.label}</Badge>
                    </Td>
                    <Td className="text-right">
                      {key.revoked_at !== null ? (
                        <span className="text-xs text-subtle">已撤销</span>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => setRevoking(key)}
                        >
                          撤销
                        </Button>
                      )}
                    </Td>
                  </tr>
                )
              })}
            </tbody>
          </Table>
        )}
        {apiKeys.hasNextPage && (
          <div className="flex justify-center border-t border-border px-4 py-3">
            <Button
              size="sm"
              loading={apiKeys.isFetchingNextPage}
              onClick={() => void apiKeys.fetchNextPage()}
            >
              加载更多
            </Button>
          </div>
        )}
      </Card>

      <CreateApiKeyModal
        open={creating}
        organizationId={organizationId}
        onClose={() => setCreating(false)}
        onCreated={(created) => {
          setCreating(false)
          void queryClient.invalidateQueries({ queryKey: keys.apiKeys(organizationId) })
          onReveal({
            title: 'API Key 已创建',
            description: `${created.key.name} · scope ${created.key.scopes.join(', ')}`,
            secret: created.plaintext,
          })
        }}
      />

      <ConfirmModal
        open={revoking !== null}
        title="撤销 API Key"
        destructive
        confirmLabel="撤销"
        loading={busy}
        description={`撤销后使用该 Key 的调用会立即返回 401。此操作不可撤销。`}
        onConfirm={revoke}
        onClose={() => setRevoking(null)}
      />
    </>
  )
}

function CreateApiKeyModal({
  open,
  organizationId,
  onClose,
  onCreated,
}: {
  open: boolean
  organizationId: string | null
  onClose: () => void
  onCreated: (created: Awaited<ReturnType<typeof endpoints.createApiKey>>) => void
}) {
  const toast = useToast()
  const [name, setName] = useState('')
  const [scopes, setScopes] = useState<string[]>(DEFAULT_SCOPES)
  const [expiry, setExpiry] = useState('2592000')
  const [busy, setBusy] = useState(false)

  const create = async () => {
    if (!organizationId) return
    setBusy(true)
    try {
      const created = await endpoints.createApiKey(organizationId, {
        name: name.trim(),
        scopes,
        expires_in_seconds: expiry ? Number.parseInt(expiry, 10) : undefined,
      })
      setName('')
      setScopes(DEFAULT_SCOPES)
      onCreated(created)
    } catch (error) {
      toast.fromError(error, '创建 API Key 失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open={open}
      width="lg"
      title="新建 API Key"
      description="请求必须同时通过组织角色和 scope 检查。选择满足调用需求的最小 scope 集合。"
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button
            size="sm"
            variant="primary"
            loading={busy}
            disabled={!name.trim() || scopes.length === 0}
            onClick={create}
          >
            创建
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="名称" required hint="1 到 120 个字符，用于识别调用方。">
          {(id) => (
            <Input
              id={id}
              autoFocus
              value={name}
              maxLength={120}
              placeholder="例如 sdk-production"
              onChange={(event) => setName(event.target.value)}
            />
          )}
        </Field>

        <Field label="有效期">
          {(id) => (
            <Select id={id} value={expiry} onChange={(event) => setExpiry(event.target.value)}>
              {EXPIRY_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          )}
        </Field>

        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-muted">
              Scope <span className="text-subtle">({scopes.length})</span>
            </span>
            <div className="flex gap-1.5">
              <Button size="sm" variant="ghost" onClick={() => setScopes(DEFAULT_SCOPES)}>
                常用
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setScopes([])}>
                清空
              </Button>
            </div>
          </div>
          <div className="grid max-h-56 grid-cols-1 gap-1 overflow-y-auto rounded-lg border border-border bg-surface-2 p-2 sm:grid-cols-2">
            {API_KEY_SCOPES.map((scope) => (
              <label
                key={scope}
                className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-xs transition hover:bg-surface"
              >
                <input
                  type="checkbox"
                  checked={scopes.includes(scope)}
                  onChange={(event) =>
                    setScopes((current) =>
                      event.target.checked
                        ? [...current, scope]
                        : current.filter((value) => value !== scope),
                    )
                  }
                  className="size-3.5 rounded border-border-strong bg-surface accent-[var(--app-accent)]"
                />
                <code className="font-mono text-[11px]">{scope}</code>
              </label>
            ))}
          </div>
          <p className="text-[11px] leading-relaxed text-subtle">
            部分管理 scope 已为后续接口保留，当前没有对应的写路由。
          </p>
        </div>
      </div>
    </Modal>
  )
}

/* --------------------------------------------------- Worker credentials */

function WorkerCredentialsPanel({
  organizationId,
  canManage,
  onReveal,
}: {
  organizationId: string | null
  canManage: boolean
  onReveal: (secret: RevealedSecret) => void
}) {
  const credentials = useWorkerCredentials(organizationId)
  const queryClient = useQueryClient()
  const toast = useToast()
  const [creating, setCreating] = useState(false)
  const [revoking, setRevoking] = useState<WorkerCredential | null>(null)
  const [busy, setBusy] = useState(false)

  const revoke = async () => {
    if (!revoking || !organizationId) return
    setBusy(true)
    try {
      await endpoints.revokeWorkerCredential(organizationId, revoking.id)
      void queryClient.invalidateQueries({ queryKey: keys.workerCredentials(organizationId) })
      void queryClient.invalidateQueries({ queryKey: keys.devices(organizationId) })
      toast.success('Worker 凭据已撤销', '对应 Worker session 已被断开')
      setRevoking(null)
    } catch (error) {
      toast.fromError(error, '撤销失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <Card>
        <CardHeader
          title="Worker 凭据"
          description="nwk_ 凭据用于边缘设备反向连接 Hub。它既不是浏览器 session，也不是消费者 API Key。"
          actions={
            <Button size="sm" variant="primary" disabled={!canManage} onClick={() => setCreating(true)}>
              <IconPlus className="size-3.5" />
              新建
            </Button>
          }
        />
        {credentials.isLoading ? (
          <SkeletonRows rows={3} />
        ) : credentials.isError ? (
          <ErrorState
            message={(credentials.error as Error).message}
            onRetry={() => void credentials.refetch()}
          />
        ) : (credentials.data?.length ?? 0) === 0 ? (
          <EmptyState
            icon={<IconKey className="size-8" />}
            title="还没有 Worker 凭据"
            description="创建后写入 CLI 配置或 ComfyUI 节点，Worker 连接并注册 device_id 后即可在设备页看到它。"
          />
        ) : (
          <Table>
            <thead>
              <tr>
                <Th>名称</Th>
                <Th>前缀</Th>
                <Th>namespace 限制</Th>
                <Th>最后使用</Th>
                <Th>状态</Th>
                <Th className="text-right">操作</Th>
              </tr>
            </thead>
            <tbody>
              {credentials.data?.map((credential) => {
                const status = credentialStatus(credential.revoked_at, credential.expires_at)
                return (
                  <tr
                    key={credential.id}
                    className={cx('transition hover:bg-surface-2/50', status.dimmed && 'opacity-60')}
                  >
                    <Td>
                      <p className="text-xs font-medium">{credential.name}</p>
                      <p className="text-[10px] text-subtle">
                        创建于 {formatDateTime(credential.created_at)}
                      </p>
                    </Td>
                    <Td>
                      <code className="font-mono text-[11px] text-muted">
                        {credential.token_prefix}…
                      </code>
                    </Td>
                    <Td>
                      {credential.allowed_namespace ? (
                        <code className="font-mono text-[11px]">{credential.allowed_namespace}</code>
                      ) : (
                        <span className="text-xs text-subtle">不限</span>
                      )}
                    </Td>
                    <Td className="text-xs whitespace-nowrap text-muted">
                      {credential.last_used_at ? (
                        <RelativeTime value={credential.last_used_at} />
                      ) : (
                        '从未使用'
                      )}
                    </Td>
                    <Td>
                      <Badge tone={status.tone}>{status.label}</Badge>
                    </Td>
                    <Td className="text-right">
                      {credential.revoked_at !== null ? (
                        <span className="text-xs text-subtle">已撤销</span>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => setRevoking(credential)}
                        >
                          撤销
                        </Button>
                      )}
                    </Td>
                  </tr>
                )
              })}
            </tbody>
          </Table>
        )}
        {credentials.hasNextPage && (
          <div className="flex justify-center border-t border-border px-4 py-3">
            <Button
              size="sm"
              loading={credentials.isFetchingNextPage}
              onClick={() => void credentials.fetchNextPage()}
            >
              加载更多
            </Button>
          </div>
        )}
      </Card>

      <CreateWorkerCredentialModal
        open={creating}
        organizationId={organizationId}
        onClose={() => setCreating(false)}
        onCreated={(created) => {
          setCreating(false)
          void queryClient.invalidateQueries({ queryKey: keys.workerCredentials(organizationId) })
          onReveal({
            title: 'Worker 凭据已创建',
            description: `${created.credential.name} · 写入 Worker 配置的 token 字段`,
            secret: created.plaintext,
          })
        }}
      />

      <ConfirmModal
        open={revoking !== null}
        title="撤销 Worker 凭据"
        destructive
        confirmLabel="撤销"
        loading={busy}
        description="撤销会立即断开使用该凭据的 Worker session，设备将变为离线，直到你配置新的凭据。"
        onConfirm={revoke}
        onClose={() => setRevoking(null)}
      />
    </>
  )
}

function CreateWorkerCredentialModal({
  open,
  organizationId,
  onClose,
  onCreated,
}: {
  open: boolean
  organizationId: string | null
  onClose: () => void
  onCreated: (created: Awaited<ReturnType<typeof endpoints.createWorkerCredential>>) => void
}) {
  const toast = useToast()
  const [name, setName] = useState('')
  const [namespace, setNamespace] = useState('')
  const [expiry, setExpiry] = useState('')
  const [busy, setBusy] = useState(false)

  const create = async () => {
    if (!organizationId) return
    setBusy(true)
    try {
      const created = await endpoints.createWorkerCredential(organizationId, {
        name: name.trim(),
        allowed_namespace: namespace.trim() || undefined,
        expires_in_seconds: expiry ? Number.parseInt(expiry, 10) : undefined,
      })
      setName('')
      setNamespace('')
      onCreated(created)
    } catch (error) {
      toast.fromError(error, '创建 Worker 凭据失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open={open}
      title="新建 Worker 凭据"
      description="同一组织内相同 Worker identity 不能被另一用户的凭据接管。"
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button size="sm" variant="primary" loading={busy} disabled={!name.trim()} onClick={create}>
            创建
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="名称" required hint="例如设备所在主机名。">
          {(id) => (
            <Input
              id={id}
              autoFocus
              value={name}
              maxLength={120}
              placeholder="例如 studio-rtx4090"
              onChange={(event) => setName(event.target.value)}
            />
          )}
        </Field>
        <Field label="namespace 限制" hint="留空表示不限制。填写后该凭据只能注册到指定 namespace。">
          {(id) => (
            <Input
              id={id}
              value={namespace}
              placeholder="例如 studio"
              className="font-mono"
              onChange={(event) => setNamespace(event.target.value)}
            />
          )}
        </Field>
        <Field label="有效期">
          {(id) => (
            <Select id={id} value={expiry} onChange={(event) => setExpiry(event.target.value)}>
              {EXPIRY_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          )}
        </Field>
      </div>
    </Modal>
  )
}

/* ----------------------------------------------------------------- utils */

function credentialStatus(
  revokedAt: number | null,
  expiresAt: number | null,
): { label: string; tone: 'success' | 'neutral' | 'warning' | 'danger'; dimmed: boolean } {
  if (revokedAt !== null) return { label: '已撤销', tone: 'danger', dimmed: true }
  if (expiresAt !== null && expiresAt <= Date.now()) {
    return { label: '已过期', tone: 'neutral', dimmed: true }
  }
  if (expiresAt !== null && expiresAt - Date.now() < 3 * 86_400_000) {
    return { label: '即将过期', tone: 'warning', dimmed: false }
  }
  return { label: '有效', tone: 'success', dimmed: false }
}
