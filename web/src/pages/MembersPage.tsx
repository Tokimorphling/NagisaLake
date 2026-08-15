import { useState } from 'react'
import {
  useAcceptOrganizationInvite,
  useChangeMemberRole,
  useCreateOrganizationInvite,
  useMembers,
  useOrganizationInvites,
  useRemoveMember,
  useRevokeOrganizationInvite,
  useTransferOrganizationOwner,
} from '@/api/queries'
import type { OrganizationMember, Role } from '@/api/types'
import { formatDateTime } from '@/lib/format'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Page, PageHeader } from '@/components/layout/AppLayout'
import {
  Copyable,
  EmptyState,
  ErrorState,
  RoleBadge,
  SkeletonRows,
  Table,
  Td,
  Th,
} from '@/components/ui/display'
import { Button, Card, CardHeader, Field, Input, Select } from '@/components/ui/primitives'

const ROLES: Role[] = ['viewer', 'member', 'operator', 'admin', 'owner']
const INVITE_ROLES: Role[] = ['viewer', 'member', 'operator', 'admin']

const ROLE_CAPABILITIES: Array<{ label: string; roles: Role[] }> = [
  { label: 'workflow、组织作业、配额只读', roles: ['viewer', 'member', 'operator', 'admin', 'owner'] },
  { label: '上传对象、提交作业、取消自己的作业', roles: ['member', 'operator', 'admin', 'owner'] },
  { label: '注册/使用/分享自己的设备、管理自己的 Key', roles: ['member', 'operator', 'admin', 'owner'] },
  { label: '管理 Worker、取消任意作业', roles: ['operator', 'admin', 'owner'] },
  { label: '管理成员、全部 Key、配额、审计', roles: ['admin', 'owner'] },
  { label: '删除组织', roles: ['owner'] },
]

export function MembersPage() {
  const { organizationId, atLeast, user, role, reloadMemberships, switchOrganization } = useAuth()
  const canManage = atLeast('admin')
  const members = useMembers(organizationId, canManage)
  const invites = useOrganizationInvites(organizationId, canManage)
  const changeRole = useChangeMemberRole(organizationId)
  const removeMember = useRemoveMember(organizationId)
  const createInvite = useCreateOrganizationInvite(organizationId)
  const revokeInvite = useRevokeOrganizationInvite(organizationId)
  const transferOwner = useTransferOrganizationOwner(organizationId)
  const acceptInvite = useAcceptOrganizationInvite()
  const toast = useToast()
  const [pending, setPending] = useState<string | null>(null)
  const [inviteRole, setInviteRole] = useState<Role>('member')
  const [inviteDays, setInviteDays] = useState('7')
  const [createdCode, setCreatedCode] = useState<string | null>(null)
  const [acceptCode, setAcceptCode] = useState('')

  const ownerCount = members.data?.filter((member) => member.role === 'owner').length ?? 0

  const updateRole = async (member: OrganizationMember, nextRole: Role) => {
    setPending(member.user_id)
    try {
      await changeRole.mutateAsync({ userId: member.user_id, role: nextRole })
      toast.success('角色已更新', `${member.email} -> ${nextRole}`)
    } catch (error) {
      toast.fromError(error, '更新角色失败')
    } finally {
      setPending(null)
    }
  }

  const remove = async (member: OrganizationMember) => {
    if (!window.confirm(`确认移除 ${member.email}？其组织 session、API key 和 worker 凭据也会失效。`)) return
    setPending(`remove:${member.user_id}`)
    try {
      await removeMember.mutateAsync(member.user_id)
      toast.success('成员已移除', member.email)
    } catch (error) {
      toast.fromError(error, '移除成员失败')
    } finally {
      setPending(null)
    }
  }

  const transfer = async (member: OrganizationMember) => {
    if (!window.confirm(`确认将组织 owner 转移给 ${member.email}？你会降为 admin。`)) return
    setPending(`transfer:${member.user_id}`)
    try {
      await transferOwner.mutateAsync(member.user_id)
      await reloadMemberships()
      toast.success('owner 已转移', `${member.email} 现在是 owner`)
    } catch (error) {
      toast.fromError(error, '转移 owner 失败')
    } finally {
      setPending(null)
    }
  }

  const create = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const days = Number(inviteDays)
    if (!Number.isFinite(days) || days < 1 || days > 30) {
      toast.error('邀请有效期无效', '请输入 1-30 天')
      return
    }
    try {
      const result = await createInvite.mutateAsync({
        role: inviteRole,
        expires_in_seconds: Math.round(days * 24 * 60 * 60),
      })
      setCreatedCode(result.plaintext)
      toast.success('邀请已创建', '邀请码只会在这里显示一次')
    } catch (error) {
      toast.fromError(error, '创建邀请失败')
    }
  }

  const accept = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const code = acceptCode.trim()
    if (!code) return
    try {
      const membership = await acceptInvite.mutateAsync(code)
      setAcceptCode('')
      await switchOrganization(membership.organization_id)
      toast.success('已加入组织', membership.organization_name)
    } catch (error) {
      toast.fromError(error, '接受组织邀请失败')
    }
  }

  return (
    <Page>
      <PageHeader
        title="成员"
        description="组织成员通过一次性邀请码加入；移除成员会撤销其组织内的浏览器 session、API key 和 worker 凭据。"
      />

      <div className="space-y-4">
        <Card>
          <CardHeader title="接受组织邀请" description="粘贴组织管理员提供的 noi_ 邀请码。" />
          <form className="flex flex-col gap-3 p-5 sm:flex-row sm:items-end" onSubmit={(event) => void accept(event)}>
            <div className="min-w-0 flex-1">
              <Field label="邀请码" required>
                {(id) => (
                  <Input
                    id={id}
                    value={acceptCode}
                    onChange={(event) => setAcceptCode(event.target.value)}
                    placeholder="noi_..."
                    autoComplete="off"
                  />
                )}
              </Field>
            </div>
            <Button type="submit" loading={acceptInvite.isPending} disabled={!acceptCode.trim()}>
              接受邀请
            </Button>
          </form>
        </Card>

        {canManage && (
          <>
            <Card>
              <CardHeader title="邀请成员" description="邀请码只保存 hash，明文只在创建成功后显示一次。" />
              <form className="grid gap-4 p-5 sm:grid-cols-[1fr_1fr_8rem_auto] sm:items-end" onSubmit={(event) => void create(event)}>
                <Field label="加入角色">
                  {(id) => (
                    <Select id={id} value={inviteRole} onChange={(event) => setInviteRole(event.target.value as Role)}>
                      {INVITE_ROLES.map((option) => (
                        <option key={option} value={option}>{option}</option>
                      ))}
                    </Select>
                  )}
                </Field>
                <Field label="有效期（天）" hint="1-30 天">
                  {(id) => (
                    <Input
                      id={id}
                      type="number"
                      min={1}
                      max={30}
                      step={1}
                      value={inviteDays}
                      onChange={(event) => setInviteDays(event.target.value)}
                    />
                  )}
                </Field>
                <div className="sm:col-span-2 sm:text-right">
                  <Button type="submit" loading={createInvite.isPending}>生成邀请码</Button>
                </div>
              </form>
              {createdCode && (
                <div className="mx-5 mb-5 rounded-lg border border-success/30 bg-success/10 p-4">
                  <p className="text-xs font-medium text-success">请立即复制，离开此页面后不会再次显示</p>
                  <Copyable value={createdCode} className="mt-2 w-full text-sm" />
                </div>
              )}
            </Card>

            <Card>
              <CardHeader title="已发出的邀请" description={`${invites.data?.length ?? 0} 条记录`} />
              {invites.isLoading ? (
                <SkeletonRows rows={2} />
              ) : invites.isError ? (
                <ErrorState message={(invites.error as Error).message} onRetry={() => void invites.refetch()} />
              ) : (invites.data?.length ?? 0) === 0 ? (
                <EmptyState title="还没有组织邀请" />
              ) : (
                <Table>
                  <thead>
                    <tr><Th>邀请码</Th><Th>角色</Th><Th>状态</Th><Th>过期时间</Th><Th className="text-right">操作</Th></tr>
                  </thead>
                  <tbody>
                    {invites.data?.map((invite) => {
                      const active = !invite.accepted_at && !invite.revoked_at && invite.expires_at > Date.now()
                      return (
                        <tr key={invite.id}>
                          <Td><span className="font-mono text-xs">{invite.code_prefix}...</span></Td>
                          <Td><RoleBadge role={invite.role} /></Td>
                          <Td className="text-xs text-muted">{invite.accepted_at ? '已接受' : invite.revoked_at ? '已撤销' : active ? '有效' : '已过期'}</Td>
                          <Td className="whitespace-nowrap text-xs text-muted">{formatDateTime(invite.expires_at)}</Td>
                          <Td className="text-right">
                            {active && (
                              <Button
                                size="sm"
                                variant="danger"
                                loading={revokeInvite.isPending && revokeInvite.variables === invite.id}
                                onClick={() => void revokeInvite.mutateAsync(invite.id).catch((error) => toast.fromError(error, '撤销邀请失败'))}
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
            </Card>
          </>
        )}

        {!canManage ? (
          <Card>
            <EmptyState title="成员管理需要 admin 或 owner 角色" description="接受邀请仍然可用；成员列表和管理操作由组织管理员执行。" />
          </Card>
        ) : (
          <Card>
            <CardHeader title="组织成员" description={`${members.data?.length ?? 0} 人`} />
            {members.isLoading ? (
              <SkeletonRows rows={3} />
            ) : members.isError ? (
              <ErrorState message={(members.error as Error).message} onRetry={() => void members.refetch()} />
            ) : (members.data?.length ?? 0) === 0 ? (
              <EmptyState title="没有成员记录" />
            ) : (
              <Table>
                <thead>
                  <tr><Th>邮箱</Th><Th>当前角色</Th><Th>加入时间</Th><Th className="text-right">管理</Th></tr>
                </thead>
                <tbody>
                  {members.data?.map((member) => {
                    const isSelf = member.user_id === user?.id
                    const isLastOwner = member.role === 'owner' && ownerCount <= 1
                    const isPending = pending === member.user_id
                    return (
                      <tr key={member.user_id} className="transition hover:bg-surface-2/50">
                        <Td>
                          <p className="text-xs font-medium">{member.email}</p>
                          {isSelf && <p className="text-[10px] text-subtle">这是你自己</p>}
                        </Td>
                        <Td><RoleBadge role={member.role} /></Td>
                        <Td className="whitespace-nowrap text-xs text-muted">{formatDateTime(member.created_at)}</Td>
                        <Td>
                          <div className="flex flex-wrap justify-end gap-2">
                            <Select
                              value={member.role}
                              aria-label={`修改 ${member.email} 的角色`}
                              disabled={isPending || isLastOwner}
                              title={isLastOwner ? '不能变更最后一个 owner' : undefined}
                              className="max-w-32"
                              onChange={(event) => void updateRole(member, event.target.value as Role)}
                            >
                              {ROLES.filter((option) => option !== 'owner' || role === 'owner').map((option) => (
                                <option key={option} value={option}>{option}</option>
                              ))}
                            </Select>
                            {role === 'owner' && !isSelf && member.role !== 'owner' && (
                              <Button
                                size="sm"
                                loading={pending === `transfer:${member.user_id}`}
                                onClick={() => void transfer(member)}
                              >
                                转移 owner
                              </Button>
                            )}
                            {!isSelf && !(member.role === 'owner' && role !== 'owner') && (
                              <Button
                                size="sm"
                                variant="danger"
                                loading={pending === `remove:${member.user_id}`}
                                disabled={member.role === 'owner' && ownerCount <= 1}
                                onClick={() => void remove(member)}
                              >
                                移除
                              </Button>
                            )}
                          </div>
                        </Td>
                      </tr>
                    )
                  })}
                </tbody>
              </Table>
            )}
          </Card>
        )}

        {canManage && (
          <Card>
            <CardHeader title="权限矩阵" description="服务端授权策略，实际操作仍以 API 返回为准。" />
            <Table>
              <thead><tr><Th>能力</Th>{ROLES.map((option) => <Th key={option} className="text-center">{option}</Th>)}</tr></thead>
              <tbody>
                {ROLE_CAPABILITIES.map((capability) => (
                  <tr key={capability.label}>
                    <Td className="text-xs text-muted">{capability.label}</Td>
                    {ROLES.map((option) => (
                      <Td key={option} className="text-center">
                        {capability.roles.includes(option) ? <span className="text-success" aria-label="允许">✓</span> : <span className="text-subtle" aria-label="不允许">-</span>}
                      </Td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </Table>
          </Card>
        )}
      </div>
    </Page>
  )
}
