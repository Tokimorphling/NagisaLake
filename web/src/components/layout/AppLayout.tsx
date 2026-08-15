import { useEffect, useState } from 'react'
import { NavLink, Outlet, useLocation } from 'react-router-dom'
import type { Role } from '@/api/types'
import { useAuth } from '@/state/auth'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { Button, cx } from '@/components/ui/primitives'
import { OrgSwitcher } from './OrgSwitcher'
import { CommandPalette } from '@/components/ui/CommandPalette'
import {
  IconAudit,
  IconDashboard,
  IconDevice,
  IconGallery,
  IconJobs,
  IconKey,
  IconLogo,
  IconLogout,
  IconMembers,
  IconMenu,
  IconMoon,
  IconQuota,
  IconSearch,
  IconSettings,
  IconSun,
  IconWorkflow,
} from './icons'

interface NavItem {
  to: string
  label: string
  icon: (props: { className?: string }) => React.ReactElement
  /** Menu hint only. The server's 403 is always the real boundary. */
  minRole?: Role
}

const NAV_GROUPS: Array<{ label: string; items: NavItem[] }> = [
  {
    label: '工作台',
    items: [
      { to: '/', label: '概览', icon: IconDashboard },
      { to: '/workflows', label: 'Workflow', icon: IconWorkflow },
      { to: '/jobs', label: '作业', icon: IconJobs },
      { to: '/gallery', label: '公共 Gallery', icon: IconGallery },
    ],
  },
  {
    label: '算力',
    items: [
      { to: '/devices', label: '设备', icon: IconDevice, minRole: 'member' },
      { to: '/credentials', label: '凭据', icon: IconKey, minRole: 'member' },
    ],
  },
  {
    label: '组织',
    items: [
      { to: '/members', label: '成员', icon: IconMembers, minRole: 'admin' },
      { to: '/quota', label: '配额', icon: IconQuota },
      { to: '/audit', label: '审计', icon: IconAudit, minRole: 'admin' },
      { to: '/settings', label: '设置', icon: IconSettings },
    ],
  },
]

export function AppLayout() {
  const { user, atLeast, logout } = useAuth()
  const { resolved, setTheme } = useTheme()
  const toast = useToast()
  const location = useLocation()
  const [mobileOpen, setMobileOpen] = useState(false)
  const [cmdOpen, setCmdOpen] = useState(false)

  useEffect(() => setMobileOpen(false), [location.pathname])

  const isMac =
    typeof navigator !== 'undefined' && /Mac|iPhone|iPad/i.test(navigator.platform)
  const shortcutKey = isMac ? '⌘K' : 'Ctrl+K'

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        setCmdOpen((prev) => !prev)
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [])

  const sidebar = (
    <div className="flex h-full flex-col gap-3.5 p-3.5">
      <div className="flex items-center gap-2.5 px-2 pt-1.5">
        <IconLogo className="size-8 drop-shadow-md" />
        <div className="min-w-0">
          <p className="truncate text-sm font-bold tracking-tight bg-gradient-to-r from-text to-text/80 bg-clip-text text-transparent">
            Nagisalake
          </p>
          <div className="flex items-center gap-1.5">
            <span className="size-1.5 rounded-full bg-accent animate-pulse" aria-hidden="true" />
            <p className="text-[10px] font-mono text-subtle">控制台 v0.1</p>
          </div>
        </div>
      </div>

      <OrgSwitcher />

      {/* Cmd+K Quick Search Button */}
      <button
        type="button"
        onClick={() => setCmdOpen(true)}
        className="flex w-full items-center justify-between rounded-xl border border-border/70 bg-surface-2/60 px-3 py-2 text-xs text-subtle transition hover:border-accent/40 hover:bg-surface-2 hover:text-text shadow-xs"
      >
        <span className="flex items-center gap-2">
          <IconSearch className="size-3.5 text-accent" />
          搜索...
        </span>
        <kbd className="rounded border border-border-strong/50 bg-surface px-1.5 py-0.5 font-mono text-[10px] text-subtle">
          {shortcutKey}
        </kbd>
      </button>

      <nav className="flex-1 space-y-4 overflow-y-auto pr-1" aria-label="主导航">
        {NAV_GROUPS.map((group) => (
          <div key={group.label} className="space-y-1">
            <p className="px-2.5 pb-1 text-[10px] font-mono font-semibold tracking-wider text-subtle/80 uppercase">
              {group.label}
            </p>
            {group.items.map((item) => {
              const permitted = !item.minRole || atLeast(item.minRole)
              return (
                <NavLink
                  key={item.to}
                  to={item.to}
                  end={item.to === '/'}
                  title={permitted ? undefined : `需要 ${item.minRole} 或更高角色`}
                  className={({ isActive }) =>
                    cx(
                      'group relative flex items-center gap-2.5 rounded-xl px-3 py-2 text-xs transition-all duration-200',
                      isActive
                        ? 'bg-accent/15 font-semibold text-accent shadow-xs border border-accent/25 backdrop-blur-md'
                        : 'text-muted hover:bg-surface-2/80 hover:text-text hover:translate-x-0.5',
                      !permitted && 'opacity-45',
                    )
                  }
                >
                  <item.icon className="size-4 shrink-0 transition-transform duration-200 group-hover:scale-110" />
                  <span className="truncate">{item.label}</span>
                </NavLink>
              )
            })}
          </div>
        ))}
      </nav>

      <div className="space-y-2.5 border-t border-border/60 pt-3">
        <div className="flex items-center gap-2.5 px-2 py-1 rounded-xl bg-surface-2/40 border border-border/40">
          <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-gradient-to-tr from-violet to-accent text-[10px] font-bold text-accent-fg shadow-sm">
            {(user?.email ?? '?').slice(0, 2).toUpperCase()}
          </span>
          <p className="min-w-0 flex-1 truncate text-[11px] font-mono text-muted" title={user?.email}>
            {user?.email}
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            size="sm"
            variant="ghost"
            className="flex-1 rounded-xl hover:bg-surface-2"
            onClick={() => setTheme(resolved === 'dark' ? 'light' : 'dark')}
            aria-label={resolved === 'dark' ? '切换到浅色主题' : '切换到深色主题'}
          >
            {resolved === 'dark' ? (
              <span className="flex items-center gap-1.5 text-xs text-muted">
                <IconSun className="size-4 text-warning" />
                浅色
              </span>
            ) : (
              <span className="flex items-center gap-1.5 text-xs text-muted">
                <IconMoon className="size-4 text-violet" />
                深色
              </span>
            )}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="flex-1 rounded-xl hover:bg-danger/10 hover:text-danger"
            aria-label="退出登录"
            onClick={async () => {
              await logout()
              toast.info('已退出登录')
            }}
          >
            <IconLogout className="size-4" />
            退出
          </Button>
        </div>
      </div>
    </div>
  )

  return (
    <div className="min-h-dvh aurora lg:grid lg:grid-cols-[15rem_1fr]">
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-[60] focus:rounded-lg focus:bg-surface focus:px-3 focus:py-2 focus:text-sm focus:text-text focus:shadow-lg"
      >
        跳到主内容
      </a>
      <aside className="sticky top-0 hidden h-dvh border-r border-border/80 bg-surface/90 backdrop-blur-xl lg:block">
        {sidebar}
      </aside>

      {/* Mobile drawer */}
      <header className="sticky top-0 z-30 flex items-center gap-3 border-b border-border/80 bg-surface/85 px-4 py-3 backdrop-blur-xl lg:hidden">
        <Button variant="ghost" size="sm" onClick={() => setMobileOpen(true)} aria-label="打开导航">
          <IconMenu className="size-4" />
        </Button>
        <IconLogo className="size-6" />
        <span className="text-sm font-semibold tracking-tight">Nagisalake</span>
      </header>

      {mobileOpen && (
        <div className="fixed inset-0 z-40 lg:hidden">
          <div
            className="absolute inset-0 bg-black/60 backdrop-blur-md transition-opacity"
            onClick={() => setMobileOpen(false)}
            aria-hidden="true"
          />
          <div className="relative h-full w-60 border-r border-border bg-surface/95 backdrop-blur-2xl">{sidebar}</div>
        </div>
      )}

      <main id="main" tabIndex={-1} className="min-w-0 focus:outline-none">
        <Outlet />
      </main>

      <CommandPalette open={cmdOpen} onClose={() => setCmdOpen(false)} />
    </div>
  )
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string
  description?: React.ReactNode
  actions?: React.ReactNode
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-4 pb-6">
      <div className="min-w-0">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {description && (
          <p className="mt-1.5 max-w-2xl text-xs leading-relaxed text-muted">{description}</p>
        )}
      </div>
      {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
    </div>
  )
}

export function Page({ children }: { children: React.ReactNode }) {
  return <div className="mx-auto w-full max-w-6xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">{children}</div>
}
