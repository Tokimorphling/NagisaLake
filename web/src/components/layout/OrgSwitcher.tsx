import { useEffect, useRef, useState } from 'react'
import { endpoints } from '@/api/endpoints'
import { useAuth } from '@/state/auth'
import { useToast } from '@/state/toast'
import { RoleBadge } from '@/components/ui/display'
import { Button, Input, cx } from '@/components/ui/primitives'
import { Modal } from '@/components/ui/Modal'
import { IconChevron, IconPlus } from './icons'

export function OrgSwitcher() {
  const { memberships, organizationId, currentMembership, switchOrganization, reloadMemberships } =
    useAuth()
  const toast = useToast()
  const [open, setOpen] = useState(false)
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [open])

  const create = async () => {
    setBusy(true)
    try {
      const membership = await endpoints.createOrganization(name.trim())
      await reloadMemberships()
      await switchOrganization(membership.organization_id)
      toast.success('组织已创建', membership.organization_name)
      setCreating(false)
      setName('')
    } catch (error) {
      toast.fromError(error, '创建组织失败')
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <div ref={containerRef} className="relative">
        <button
          type="button"
          onClick={() => setOpen((value) => !value)}
          aria-expanded={open}
          aria-haspopup="listbox"
          className={cx(
            'flex w-full items-center gap-2 rounded-lg border border-border bg-surface-2 px-2.5 py-2',
            'text-left transition hover:border-border-strong hover:bg-elevated',
          )}
        >
          <span className="grid size-7 shrink-0 place-items-center rounded-md bg-accent/15 text-xs font-semibold text-accent">
            {(currentMembership?.organization_name ?? '?').slice(0, 1).toUpperCase()}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-xs font-medium">
              {currentMembership?.organization_name ?? '未选择组织'}
            </span>
            <span className="block text-[10px] text-subtle">
              {currentMembership?.role ?? '—'}
            </span>
          </span>
          <IconChevron className={cx('size-3.5 shrink-0 text-subtle transition', open && 'rotate-90')} />
        </button>

        {open && (
          <div
            role="listbox"
            className="absolute z-40 mt-1.5 w-full overflow-hidden rounded-lg border border-border bg-elevated shadow-[var(--shadow-card)]"
          >
            <div className="max-h-64 overflow-y-auto p-1">
              {memberships.map((membership) => {
                const active = membership.organization_id === organizationId
                return (
                  <button
                    key={membership.organization_id}
                    type="button"
                    role="option"
                    aria-selected={active}
                    onClick={async () => {
                      setOpen(false)
                      if (active) return
                      try {
                        await switchOrganization(membership.organization_id)
                        toast.info('已切换组织', membership.organization_name)
                      } catch (error) {
                        toast.fromError(error, '切换组织失败')
                      }
                    }}
                    className={cx(
                      'flex w-full items-center justify-between gap-2 rounded-md px-2.5 py-2 text-left transition',
                      active ? 'bg-accent/10 text-accent' : 'hover:bg-surface-2',
                    )}
                  >
                    <span className="min-w-0 truncate text-xs">{membership.organization_name}</span>
                    <RoleBadge role={membership.role} />
                  </button>
                )
              })}
            </div>
            <div className="border-t border-border p-1">
              <button
                type="button"
                onClick={() => {
                  setOpen(false)
                  setCreating(true)
                }}
                className="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-xs text-muted transition hover:bg-surface-2 hover:text-text"
              >
                <IconPlus className="size-3.5" />
                新建组织
              </button>
            </div>
          </div>
        )}
      </div>

      <Modal
        open={creating}
        title="新建组织"
        description="你会成为该组织的 owner。组织之间的设备、Key 和配额完全隔离。"
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button size="sm" onClick={() => setCreating(false)} disabled={busy}>
              取消
            </Button>
            <Button
              size="sm"
              variant="primary"
              loading={busy}
              disabled={!name.trim()}
              onClick={create}
            >
              创建
            </Button>
          </>
        }
      >
        <label className="space-y-1.5 text-xs font-medium text-muted">
          组织名称
          <Input
            autoFocus
            value={name}
            maxLength={120}
            placeholder="例如 studio-team"
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && name.trim() && !busy) void create()
            }}
          />
        </label>
      </Modal>
    </>
  )
}
