import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { Button, cx } from '@/components/ui/primitives'
import {
  IconAudio,
  IconBell,
  IconGallery,
  IconGrid,
  IconImage,
  IconLogo,
  IconLogout,
  IconSettings,
  IconSparkles,
  IconVideo,
} from './icons'

const MEDIA_NAV = [
  { to: '/studio/avatar', label: '数字人', icon: IconSparkles },
  { to: '/studio/video', label: '视频', icon: IconVideo },
  { to: '/studio/image', label: '图片', icon: IconImage },
  { to: '/studio/audio', label: '音频', icon: IconAudio },
]

export function StudioLayout() {
  const { user, logout } = useAuth()
  const toast = useToast()
  const navigate = useNavigate()
  const location = useLocation()

  return (
    <div className="studio-shell grid h-dvh min-h-0 grid-cols-[64px_1fr] overflow-hidden bg-bg">
      <aside className="flex min-h-0 flex-col items-center border-r border-border/80 bg-surface/95 py-3">
        <NavLink to="/studio/video" aria-label="NagisaLake Studio" className="mb-6 grid size-10 place-items-center rounded-xl bg-accent/10 text-accent transition hover:bg-accent/20">
          <IconLogo className="size-7" />
        </NavLink>
        <nav className="flex flex-1 flex-col items-center gap-2" aria-label="Studio 导航">
          <NavLink
            to="/studio/video"
            className={({ isActive }) => cx('grid size-10 place-items-center rounded-xl transition', isActive ? 'bg-accent/15 text-accent' : 'text-subtle hover:bg-surface-2 hover:text-text')}
            title="创作"
          >
            <IconSparkles className="size-4" />
          </NavLink>
          <NavLink
            to="/gallery"
            className={({ isActive }) => cx('grid size-10 place-items-center rounded-xl transition', isActive ? 'bg-accent/15 text-accent' : 'text-subtle hover:bg-surface-2 hover:text-text')}
            title="资产"
          >
            <IconGallery className="size-4" />
          </NavLink>
          <NavLink
            to="/console"
            className="grid size-10 place-items-center rounded-xl text-subtle transition hover:bg-surface-2 hover:text-text"
            title="Console"
          >
            <IconGrid className="size-4" />
          </NavLink>
        </nav>
        <div className="flex flex-col items-center gap-2 border-t border-border/70 pt-3">
          <button type="button" className="grid size-10 place-items-center rounded-xl text-subtle transition hover:bg-surface-2 hover:text-text" title="通知">
            <IconBell className="size-4" />
          </button>
          <button type="button" className="grid size-10 place-items-center rounded-xl text-subtle transition hover:bg-surface-2 hover:text-text" title={user?.email ?? '账户'}>
            <span className="grid size-6 place-items-center rounded-full bg-violet/25 text-[10px] font-semibold text-violet">
              {(user?.email ?? '?').slice(0, 2).toUpperCase()}
            </span>
          </button>
          <Button
            size="sm"
            variant="ghost"
            className="size-10 rounded-xl p-0"
            title="退出登录"
            onClick={async () => {
              await logout()
              toast.info('已退出登录')
              navigate('/login')
            }}
          >
            <IconLogout className="size-4" />
          </Button>
        </div>
      </aside>

      <div className="grid min-h-0 min-w-0 grid-rows-[56px_1fr]">
        <header className="flex min-w-0 items-center justify-between border-b border-border/80 bg-surface/90 px-5 backdrop-blur-xl">
          <div className="flex min-w-0 items-center gap-6 overflow-x-auto">
            <span className="shrink-0 text-sm font-semibold tracking-tight">Studio</span>
            <nav className="flex h-full items-center gap-1" aria-label="媒体类型">
              {MEDIA_NAV.map((item) => {
                const Icon = item.icon
                return (
                  <NavLink
                    key={item.to}
                    to={item.to}
                    className={({ isActive }) => cx('flex h-10 items-center gap-2 rounded-lg px-3 text-xs transition', isActive ? 'bg-accent/12 font-semibold text-accent' : 'text-muted hover:bg-surface-2 hover:text-text')}
                  >
                    <Icon className="size-3.5" />
                    {item.label}
                  </NavLink>
                )
              })}
            </nav>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <span className="hidden text-[11px] text-subtle sm:inline">{location.pathname.startsWith('/studio') ? '创作空间' : ''}</span>
            <NavLink to="/gallery" className="hidden items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs text-muted transition hover:bg-surface-2 hover:text-text sm:flex">
              <IconGallery className="size-3.5" />
              历史
            </NavLink>
            <NavLink to="/settings" className="grid size-8 place-items-center rounded-lg text-subtle transition hover:bg-surface-2 hover:text-text" title="设置">
              <IconSettings className="size-3.5" />
            </NavLink>
          </div>
        </header>
        <main className="min-h-0 min-w-0 overflow-hidden">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
