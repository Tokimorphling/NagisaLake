import { useEffect, useState, type ReactNode, type Ref } from 'react'
import { NavLink } from 'react-router-dom'
import { IconClose, IconSparkles } from '@/components/layout/icons'
import { cx } from '@/components/ui/primitives'
import { MEDIA_LABELS, STUDIO_MEDIA } from './catalog/models'

export function MediaTabs() {
  return <nav className="studio-media-tabs" aria-label="媒体类型">
    {STUDIO_MEDIA.map((media) => <NavLink key={media} to={`/studio/${media}`} className={({ isActive }) => cx('studio-media-tab', isActive && 'is-active')}>{MEDIA_LABELS[media]}</NavLink>)}
  </nav>
}

export function PromptEditor({ value, onChange, label = '提示词', placeholder, textareaRef, controls, disabled = false, onSubmit }: {
  value: string; onChange: (text: string) => void; label?: string; placeholder: string; textareaRef?: Ref<HTMLTextAreaElement>; controls?: ReactNode; disabled?: boolean; onSubmit?: () => void
}) {
  return <div className="studio-editor-group">
    <div className="mb-2.5 flex items-center justify-between gap-3 text-xs text-muted">
      <label htmlFor="studio-prompt" className="flex items-center gap-2 font-medium">{label}<IconSparkles className="size-3.5 text-subtle" /></label>
      <span className="tabular-nums text-subtle">{value.length.toLocaleString()} 字符</span>
    </div>
    <div className="studio-prompt-editor">
      <textarea ref={textareaRef} id="studio-prompt" value={value} onChange={(event) => onChange(event.target.value)} placeholder={placeholder} disabled={disabled} spellCheck={false} onKeyDown={(event) => { if ((event.ctrlKey || event.metaKey) && event.key === 'Enter' && !event.nativeEvent.isComposing && onSubmit) { event.preventDefault(); onSubmit() } }} />
      <div className="studio-editor-tools">
        <div className="min-w-0 flex-1">{controls}</div>
        <button type="button" className="studio-text-action" disabled={!value || disabled} onClick={() => onChange('')}><IconClose className="size-3.5" />清空</button>
      </div>
    </div>
  </div>
}

export function useDesktopStudio() {
  const [desktop, setDesktop] = useState(() => typeof window.matchMedia === 'function' && window.matchMedia('(min-width: 1024px)').matches)
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const query = window.matchMedia('(min-width: 1024px)')
    const update = () => setDesktop(query.matches)
    update()
    query.addEventListener('change', update)
    return () => query.removeEventListener('change', update)
  }, [])
  return desktop
}

export function MobilePaneSwitch({ value, onChange, resultLabel = '结果与历史' }: {
  value: 'composer' | 'results'; onChange: (value: 'composer' | 'results') => void; resultLabel?: string
}) {
  return <div className="studio-mobile-switch" role="group" aria-label="工作区面板">
    <button type="button" aria-pressed={value === 'composer'} onClick={() => onChange('composer')}>创作</button>
    <button type="button" aria-pressed={value === 'results'} onClick={() => onChange('results')}>{resultLabel}</button>
  </div>
}
