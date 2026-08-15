import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '@/state/auth'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { useJobs, useWorkflows } from '@/api/queries'
import { fuzzyScore } from '@/lib/fuzzy'
import { cx } from './primitives'
import {
  IconAudit,
  IconClose,
  IconDashboard,
  IconDevice,
  IconGallery,
  IconJobs,
  IconKey,
  IconMembers,
  IconMoon,
  IconQuota,
  IconSearch,
  IconSettings,
  IconSun,
  IconWorkflow,
} from '@/components/layout/icons'

interface CommandItem {
  id: string
  label: string
  description?: string
  icon: (props: { className?: string }) => React.ReactElement
  category: '页面' | 'Workflow' | '作业' | '组织' | '设置'
  action: () => void | Promise<void>
}

export function CommandPalette({
  open,
  onClose,
}: {
  open: boolean
  onClose: () => void
}) {
  const navigate = useNavigate()
  const { organizationId, memberships, atLeast, switchOrganization } = useAuth()
  const { resolved, setTheme } = useTheme()
  const toast = useToast()
  const workflows = useWorkflows(organizationId)
  const jobs = useJobs(organizationId)

  const [query, setQuery] = useState('')
  const [selectedIndex, setSelectedIndex] = useState(0)

  // Reset query on open
  useEffect(() => {
    if (open) {
      setQuery('')
      setSelectedIndex(0)
    }
  }, [open])

  const items = useMemo<CommandItem[]>(() => {
    const list: CommandItem[] = [
      {
        id: 'nav-dash',
        label: '概览 Dashboard',
        description: '查看算力统计与最新作业概况',
        icon: IconDashboard,
        category: '页面',
        action: () => navigate('/'),
      },
      {
        id: 'nav-workflows',
        label: 'Workflow 目录',
        description: '浏览与发起 Workflow 作业',
        icon: IconWorkflow,
        category: '页面',
        action: () => navigate('/workflows'),
      },
      {
        id: 'nav-jobs',
        label: '作业列表 Jobs',
        description: '管理所有运行与历史作业',
        icon: IconJobs,
        category: '页面',
        action: () => navigate('/jobs'),
      },
      {
        id: 'nav-gallery',
        label: '公共 Gallery',
        description: '浏览与管理共享的多媒体参数卡',
        icon: IconGallery,
        category: '页面',
        action: () => navigate('/gallery'),
      },
      {
        id: 'nav-devices',
        label: '设备管理 Devices',
        description: '管理 Compute Worker 算力节点',
        icon: IconDevice,
        category: '页面',
        action: () => navigate('/devices'),
      },
      {
        id: 'nav-credentials',
        label: '凭据管理 Credentials',
        description: '查看 Worker 注册与 Token 凭据',
        icon: IconKey,
        category: '页面',
        action: () => navigate('/credentials'),
      },
    ]

    if (atLeast('admin')) {
      list.push(
        {
          id: 'nav-members',
          label: '成员管理 Members',
          description: '管理组织成员与角色权限',
          icon: IconMembers,
          category: '页面',
          action: () => navigate('/members'),
        },
        {
          id: 'nav-audit',
          label: '审计日志 Audit Logs',
          description: '查看安全与操作审计历史',
          icon: IconAudit,
          category: '页面',
          action: () => navigate('/audit'),
        },
      )
    }

    list.push(
      {
        id: 'nav-quota',
        label: '配额管理 Quota',
        description: '查看算力使用额度',
        icon: IconQuota,
        category: '页面',
        action: () => navigate('/quota'),
      },
      {
        id: 'nav-settings',
        label: '组织设置 Settings',
        description: '更新组织配置与偏好',
        icon: IconSettings,
        category: '页面',
        action: () => navigate('/settings'),
      },
      {
        id: 'theme-toggle',
        label: resolved === 'dark' ? '切换为浅色主题 Light Theme' : '切换为深色主题 Dark Theme',
        description: '变更全局界面色彩模式',
        icon: resolved === 'dark' ? IconSun : IconMoon,
        category: '设置',
        action: () => setTheme(resolved === 'dark' ? 'light' : 'dark'),
      },
    )

    memberships.forEach((membership) => {
      const active = membership.organization_id === organizationId
      list.push({
        id: `org-${membership.organization_id}`,
        label: `${active ? '当前组织' : '切换组织'}: ${membership.organization_name}`,
        description: `${membership.role} · ${membership.organization_id}`,
        icon: IconMembers,
        category: '组织',
        action: async () => {
          if (active) return
          await switchOrganization(membership.organization_id)
          toast.info('已切换组织', membership.organization_name)
        },
      })
    })

    // Append workflows
    if (workflows.data) {
      workflows.data.forEach((wf) => {
        list.push({
          id: `wf-${wf.id}`,
          label: `Workflow: ${wf.manifest?.display_name || wf.id}`,
          description: wf.manifest?.description || `版本 ${wf.version}`,
          icon: IconWorkflow,
          category: 'Workflow',
          action: () => navigate(`/workflows/${encodeURIComponent(wf.id)}`),
        })
      })
    }

    // Append recent jobs
    const recent = jobs.data?.pages.flatMap((page) => page.items).slice(0, 50) ?? []
    recent.forEach((j) => {
      list.push({
        id: `job-${j.id}`,
        label: `作业: ${j.id.slice(0, 14)}… (${j.workflow_id})`,
        description: `状态: ${j.state}`,
        icon: IconJobs,
        category: '作业',
        action: () => navigate(`/jobs/${j.id}`),
      })
    })

    return list
  }, [
    atLeast,
    jobs.data,
    memberships,
    navigate,
    organizationId,
    resolved,
    setTheme,
    switchOrganization,
    toast,
    workflows.data,
  ])

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return items
    return items
      .map((item, index) => ({
        item,
        index,
        score: fuzzyScore(
          `${item.label} ${item.description ?? ''} ${item.category}`,
          q,
        ),
      }))
      .filter((entry): entry is typeof entry & { score: number } => entry.score !== null)
      .sort((left, right) => left.score - right.score || left.index - right.index)
      .map((entry) => entry.item)
  }, [items, query])

  const grouped = useMemo(() => {
    const order: CommandItem['category'][] = ['页面', 'Workflow', '作业', '组织', '设置']
    const groups = new Map<CommandItem['category'], { item: CommandItem; index: number }[]>()
    filtered.forEach((item, index) => {
      const list = groups.get(item.category) ?? []
      list.push({ item, index })
      groups.set(item.category, list)
    })
    return order
      .filter((category) => groups.has(category))
      .map((category) => ({ category, items: groups.get(category)! }))
  }, [filtered])

  const execute = (item: CommandItem) => {
    onClose()
    void Promise.resolve(item.action()).catch((error) => {
      toast.fromError(error, '命令执行失败')
    })
  }

  useEffect(() => {
    setSelectedIndex(0)
  }, [query])

  useEffect(() => {
    if (!open) return
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setSelectedIndex((i) => (filtered.length === 0 ? 0 : Math.min(filtered.length - 1, i + 1)))
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        setSelectedIndex((i) => Math.max(0, i - 1))
      } else if (e.key === 'Enter') {
        e.preventDefault()
        if (filtered[selectedIndex]) {
          execute(filtered[selectedIndex])
        }
      } else if (e.key === 'Escape') {
        onClose()
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [filtered, onClose, open, selectedIndex, toast])

  if (!open) return null

  const isMac =
    typeof navigator !== 'undefined' && /Mac|iPhone|iPad/i.test(navigator.platform)
  const shortcutKey = isMac ? '⌘K' : 'Ctrl+K'

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center pt-16 sm:pt-24 px-4"
      role="dialog"
      aria-modal="true"
      aria-label="命令面板"
    >
      {/* Backdrop */}
      <div
        className="fixed inset-0 bg-black/60 backdrop-blur-md animate-fade-in-up"
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Modal Card */}
      <div className="relative z-10 w-full max-w-xl overflow-hidden rounded-2xl border border-border/80 bg-surface/95 shadow-2xl backdrop-blur-2xl animate-scale-in">
        {/* Input Header */}
        <div className="flex items-center gap-3 border-b border-border/60 px-4 py-3.5 bg-surface-2/30">
          <IconSearch className="size-5 text-accent shrink-0" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索页面、Workflow、作业、设置... (↑↓ 选择，Enter 跳转)"
            aria-label="搜索命令"
            className="flex-1 bg-transparent text-sm text-text placeholder:text-subtle focus:outline-none"
            autoFocus
          />
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭命令面板"
            className="inline-flex size-7 items-center justify-center rounded-lg text-subtle hover:bg-surface-2 hover:text-text transition"
          >
            <IconClose className="size-4" />
          </button>
        </div>

        {/* Results List */}
        <div className="max-h-[60vh] overflow-y-auto p-2">
          {filtered.length === 0 ? (
            <div className="flex flex-col items-center justify-center gap-3 px-6 py-12 text-center">
              <div className="grid size-10 place-items-center rounded-xl border border-border-strong/40 bg-surface-2 text-subtle">
                <IconSearch className="size-5" />
              </div>
              <div className="space-y-1">
                <p className="text-sm font-medium">没有找到匹配项</p>
                <p className="mx-auto max-w-xs text-xs leading-relaxed text-muted">
                  {query ? `试试更短的关键词，或检查拼写。“${query}”没有命中任何页面、Workflow 或作业。` : '输入页面名称、Workflow、作业 ID 或组织来快速跳转。'}
                </p>
              </div>
            </div>
          ) : (
            <div className="space-y-3">
              {grouped.map((group) => (
                <section key={group.category}>
                  <h3 className="sticky top-0 z-10 bg-surface/95 px-3.5 py-1 font-mono text-[10px] font-semibold uppercase tracking-[0.16em] text-subtle backdrop-blur">
                    {group.category}
                  </h3>
                  <ul className="mt-1 space-y-1">
                    {group.items.map(({ item, index }) => {
                      const isSelected = index === selectedIndex
                      const IconComp = item.icon
                      return (
                        <li key={item.id}>
                          <button
                            type="button"
                            onClick={() => execute(item)}
                            onMouseEnter={() => setSelectedIndex(index)}
                            className={cx(
                              'flex w-full items-center gap-3 rounded-xl px-3.5 py-2.5 text-left text-xs transition-all duration-150',
                              isSelected
                                ? 'bg-accent/15 text-accent font-medium shadow-xs border border-accent/30'
                                : 'text-text hover:bg-surface-2/70',
                            )}
                          >
                            <div
                              className={cx(
                                'grid size-8 shrink-0 place-items-center rounded-lg border transition',
                                isSelected
                                  ? 'border-accent/40 bg-accent/20 text-accent'
                                  : 'border-border-strong/40 bg-surface-2 text-muted',
                              )}
                            >
                              <IconComp className="size-4" />
                            </div>
                            <div className="min-w-0 flex-1">
                              <p className="truncate font-semibold tracking-tight">{item.label}</p>
                              {item.description && (
                                <p className="truncate text-[11px] text-muted">{item.description}</p>
                              )}
                            </div>
                          </button>
                        </li>
                      )
                    })}
                  </ul>
                </section>
              ))}
            </div>
          )}
        </div>

        {/* Footer info */}
        <div className="flex items-center justify-between border-t border-border/60 px-4 py-2 text-[11px] text-subtle font-mono bg-surface-2/40">
          <span>Esc 退出</span>
          <span>{shortcutKey} 随时召唤</span>
        </div>
      </div>
    </div>
  )
}
