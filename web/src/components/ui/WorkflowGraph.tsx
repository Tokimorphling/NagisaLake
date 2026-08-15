import type { Workflow } from '@/api/types'
import { cx } from './primitives'
import { IconFile, IconImage, IconSettings, IconSparkles, IconVideo } from '@/components/layout/icons'

/**
 * Manifest-derived stage chain for a workflow.
 *
 * This is deliberately NOT the ComfyUI node graph. `sanitize_workflow_catalog`
 * strips `pointer`, `node_id`, `node_type`, and `field` from every input before
 * the manifest reaches the browser, so the real topology — which node feeds
 * which — is not available client-side. What the manifest does describe is the
 * boundary: which artifacts go in, which parameters tune the run, and which
 * outputs come back. That is what this renders, labelled as such so it is not
 * mistaken for the graph itself.
 */
export function WorkflowGraph({ workflow }: { workflow: Workflow }) {
  const manifest = workflow.manifest
  if (!manifest) {
    return (
      <p className="px-5 py-8 text-center text-xs text-subtle">
        该 workflow 未上报 manifest，无法推导输入输出链路。
      </p>
    )
  }

  const artifacts = manifest.inputs.filter((input) => input.kind === 'artifact')
  const parameters = manifest.inputs.filter((input) => input.kind === 'parameter')

  const outputKind = (contentType: string) => {
    if (contentType.startsWith('image/')) return 'image' as const
    if (contentType.startsWith('video/')) return 'video' as const
    return 'file' as const
  }

  return (
    <div className="space-y-3 p-5">
      <div className="flex flex-col gap-2.5 lg:flex-row lg:items-stretch">
        {/* Stage 1: inputs */}
        <Stage
          title="输入"
          subtitle={artifacts.length > 0 ? `${artifacts.length} 个对象` : '无输入对象'}
          tone="violet"
          icon={<IconImage className="size-4" />}
        >
          {artifacts.length === 0 ? (
            <EmptyNode text="纯文本驱动" />
          ) : (
            artifacts.map((input, index) => (
              <Node
                key={input.name}
                name={input.name}
                meta={`#${index + 1} · ${input.content_type ?? input.type}`}
                required={input.required}
              />
            ))
          )}
        </Stage>

        <Connector />

        {/* Stage 2: parameters feeding the graph */}
        <Stage
          title="参数 / 采样"
          subtitle={`${parameters.length} 项可调参数`}
          tone="accent"
          icon={<IconSettings className="size-4" />}
        >
          {parameters.length === 0 ? (
            <EmptyNode text="无暴露参数" />
          ) : (
            <>
              {parameters.slice(0, 6).map((input) => (
                <Node
                  key={input.name}
                  name={input.name}
                  meta={
                    input.options.length > 0
                      ? `${input.type} · ${input.options.length} 个选项`
                      : input.type
                  }
                  required={input.required}
                />
              ))}
              {parameters.length > 6 && (
                <p className="px-2 pt-0.5 text-[10px] text-subtle">
                  还有 {parameters.length - 6} 项参数
                </p>
              )}
            </>
          )}
        </Stage>

        <Connector />

        {/* Stage 3: outputs */}
        <Stage
          title="输出"
          subtitle={`${manifest.outputs.length} 个产物`}
          tone="success"
          icon={<IconSparkles className="size-4" />}
        >
          {manifest.outputs.length === 0 ? (
            <EmptyNode text="未声明输出" />
          ) : (
            manifest.outputs.map((output) => {
              const kind = outputKind(output.content_type)
              return (
                <Node
                  key={output.name}
                  name={output.name}
                  meta={output.content_type}
                  icon={
                    kind === 'image' ? (
                      <IconImage className="size-3" />
                    ) : kind === 'video' ? (
                      <IconVideo className="size-3" />
                    ) : (
                      <IconFile className="size-3" />
                    )
                  }
                />
              )
            })
          )}
        </Stage>
      </div>

      <p className="text-[11px] leading-relaxed text-subtle">
        链路由 manifest 的输入输出声明推导。Hub 在下发目录时会移除
        <code className="mx-1 font-mono">node_id</code>
        <code className="mr-1 font-mono">node_type</code>
        等内部字段，因此这里展示的是作业边界，而不是 ComfyUI 的真实节点拓扑。
      </p>
    </div>
  )
}

const STAGE_TONES = {
  violet: 'border-violet/30 bg-violet/5 text-violet',
  accent: 'border-accent/30 bg-accent/5 text-accent',
  success: 'border-success/30 bg-success/5 text-success',
} as const

function Stage({
  title,
  subtitle,
  tone,
  icon,
  children,
}: {
  title: string
  subtitle: string
  tone: keyof typeof STAGE_TONES
  icon: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <div className="min-w-0 flex-1">
      <div
        className={cx(
          'flex h-full flex-col gap-2 rounded-xl border p-3 transition',
          STAGE_TONES[tone],
        )}
      >
        <div className="flex items-center gap-2">
          <span className="grid size-7 shrink-0 place-items-center rounded-lg border border-current/25 bg-current/10">
            {icon}
          </span>
          <div className="min-w-0">
            <p className="truncate text-xs font-semibold tracking-tight">{title}</p>
            <p className="truncate text-[10px] font-mono opacity-70">{subtitle}</p>
          </div>
        </div>
        <div className="space-y-1.5">{children}</div>
      </div>
    </div>
  )
}

function Node({
  name,
  meta,
  required,
  icon,
}: {
  name: string
  meta: string
  required?: boolean
  icon?: React.ReactNode
}) {
  return (
    <div className="rounded-lg border border-border/70 bg-surface/80 px-2.5 py-1.5 backdrop-blur-sm">
      <div className="flex items-center gap-1.5">
        {icon && <span className="shrink-0 text-muted">{icon}</span>}
        <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-text" title={name}>
          {name}
        </span>
        {required && (
          <span className="shrink-0 text-[10px] text-danger" title="必填">
            *
          </span>
        )}
      </div>
      <p className="truncate font-mono text-[10px] text-subtle" title={meta}>
        {meta}
      </p>
    </div>
  )
}

function EmptyNode({ text }: { text: string }) {
  return (
    <div className="rounded-lg border border-dashed border-border/70 px-2.5 py-2 text-center">
      <span className="text-[10px] text-subtle">{text}</span>
    </div>
  )
}

/** Directional arrow between stages: horizontal on wide screens, vertical below. */
function Connector() {
  return (
    <div className="flex shrink-0 items-center justify-center lg:w-6">
      <svg
        viewBox="0 0 24 24"
        className="size-4 rotate-90 text-border-strong lg:rotate-0"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <path d="M5 12h14M13 6l6 6-6 6" />
      </svg>
    </div>
  )
}
