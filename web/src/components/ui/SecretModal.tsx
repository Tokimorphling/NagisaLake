import { useState } from 'react'
import { copyText } from '@/lib/format'
import { Modal } from './Modal'
import { Button } from './primitives'

/**
 * Shown once for nsk_ / nwk_ / ndi_ secrets. The Hub only ever returns the
 * plaintext on creation, so the dialog makes the user acknowledge that.
 */
export function SecretModal({
  open,
  title,
  description,
  secret,
  onClose,
}: {
  open: boolean
  title: string
  description: string
  secret: string
  onClose: () => void
}) {
  const [copied, setCopied] = useState(false)
  const [acknowledged, setAcknowledged] = useState(false)

  const close = () => {
    setCopied(false)
    setAcknowledged(false)
    onClose()
  }

  return (
    <Modal
      open={open}
      title={title}
      description={description}
      onClose={close}
      footer={
        <Button size="sm" variant="primary" disabled={!acknowledged} onClick={close}>
          我已安全保存
        </Button>
      }
    >
      <div className="space-y-4">
        <div className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2.5 text-xs leading-relaxed text-warning">
          这是唯一一次显示明文的机会。关闭后列表只会显示前缀和状态，无法再次查看。
        </div>

        <div className="space-y-2">
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium text-muted">凭据明文</span>
            <Button
              size="sm"
              variant={copied ? 'secondary' : 'primary'}
              onClick={async () => setCopied(await copyText(secret))}
            >
              {copied ? '已复制' : '复制'}
            </Button>
          </div>
          <code className="block overflow-x-auto rounded-lg border border-border bg-surface-2 px-3 py-2.5 font-mono text-xs break-all">
            {secret}
          </code>
        </div>

        <label className="flex cursor-pointer items-start gap-2 text-xs leading-relaxed text-muted">
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(event) => setAcknowledged(event.target.checked)}
            className="mt-0.5 size-4 shrink-0 rounded border-border-strong bg-surface-2 accent-[var(--app-accent)]"
          />
          我已把它保存到 secret manager 或密码管理器，并确认不会提交到 Git。
        </label>
      </div>
    </Modal>
  )
}
