import { useState } from 'react'
import { Link, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { useAuth } from '@/state/auth'
import { useTheme } from '@/state/theme'
import { useToast } from '@/state/toast'
import { Button, cx } from '@/components/ui/primitives'
import { Modal } from '@/components/ui/Modal'
import { OrgSwitcher } from './OrgSwitcher'
import { IconDevice, IconGallery, IconGrid, IconJobs, IconLogo, IconLogout, IconMoon, IconSettings, IconSparkles, IconSun, IconWorkflow } from './icons'
import '@/features/studio/studio.css'

export function StudioLayout() {
  const { user, logout, currentMembership } = useAuth()
  const { resolved, setTheme } = useTheme()
  const [accountOpen, setAccountOpen] = useState(false)
  const toast = useToast()
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const agentMode = /^\/studio\/agent\/?$/.test(pathname)
  const links = [
    { to: '/gallery', label: '发现', icon: IconGallery, active: false },
    { to: '/studio/video', label: '自由创作', icon: IconSparkles, active: !agentMode },
    { to: '/studio/agent', label: 'Agent 模式', icon: IconWorkflow, active: agentMode },
    { to: '/jobs', label: '全部任务', icon: IconJobs, active: false },
    { to: '/devices', label: '设备', icon: IconDevice, active: false },
  ]
  return <div className="studio-shell">
    <a href="#studio-main" className="sr-only focus:not-sr-only focus:fixed focus:left-20 focus:top-3 focus:z-50 focus:rounded-lg focus:bg-surface focus:p-3 focus:text-sm">跳至创作区</a>
    <aside className="studio-rail" aria-label="Studio 侧栏">
      <Link to="/studio/video" className="mb-5 flex flex-col items-center gap-1.5 text-accent" aria-label="NagisaLake Studio 首页">
        <IconLogo className="size-7" /><span className="text-[8px] font-semibold tracking-wider text-muted">NAGISA</span>
      </Link>
      <nav className="flex w-full flex-1 flex-col items-center gap-2" aria-label="Studio 导航">
        {links.map(({ to, label, icon: Icon, active }) => <Link key={to} to={to} className={cx('studio-rail-link', active && 'is-active')} aria-current={active ? 'page' : undefined}><Icon /><span>{label}</span></Link>)}
        <div className="my-2 w-7 border-t border-border" />
        <Link to="/console" className="studio-rail-link"><IconGrid /><span>控制台</span></Link>
      </nav>
      <div className="mt-5 flex flex-col items-center gap-2">
        <button type="button" className="studio-rail-tool" onClick={() => setTheme(resolved === 'dark' ? 'light' : 'dark')} aria-label={resolved === 'dark' ? '切换浅色主题' : '切换深色主题'} title={resolved === 'dark' ? '切换浅色主题' : '切换深色主题'}>{resolved === 'dark' ? <IconSun className="size-4" /> : <IconMoon className="size-4" />}</button>
        <Link to="/settings" className="studio-rail-tool" aria-label="偏好与通知设置" title="偏好与通知设置"><IconSettings className="size-4" /></Link>
        <button type="button" className="studio-rail-account" onClick={() => setAccountOpen(true)} aria-label="账户与组织" title={currentMembership?.organization_name ?? '账户与组织'}>
          <span className="grid size-7 place-items-center rounded-full bg-accent/15 text-[10px] font-bold text-accent">{(user?.email ?? '?').slice(0, 2).toUpperCase()}</span>
          <span className="max-w-full truncate text-[9px] text-muted">{currentMembership?.organization_name ?? '选择组织'}</span>
        </button>
      </div>
    </aside>
    <main id="studio-main" className="min-h-0 min-w-0 overflow-hidden" tabIndex={-1}><Outlet /></main>
    <Modal open={accountOpen} title="账户与组织" description={user?.email} onClose={() => setAccountOpen(false)} footer={<Button size="sm" variant="ghost" onClick={async () => { await logout(); setAccountOpen(false); toast.info('已退出登录'); navigate('/login') }}><IconLogout className="size-3.5" />退出登录</Button>}>
      <div className="min-h-24 py-2"><OrgSwitcher /><p className="mt-4 text-xs leading-5 text-muted">工作流、任务与生成配额按组织隔离。切换组织会清空当前创作草稿并取消未完成的 Agent 请求。</p></div>
    </Modal>
  </div>
}
