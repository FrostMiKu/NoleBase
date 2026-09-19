import { useState } from 'react'
import { ChevronRight, CircleCheck, CircleDashed, CircleX, Loader2 } from 'lucide-react'

import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { useAgentStore } from '@/stores/agent-store'

/**
 * The live runtime view: assistant text (one block), a collapsed-by-default
 * thinking section, a compact tool/job timeline, runtime notices, and
 * session notifications. No chat bubbles.
 */
export function AgentTranscript() {
  const assistantText = useAgentStore((state) => state.assistantText)
  const thinkingText = useAgentStore((state) => state.thinkingText)
  const tools = useAgentStore((state) => state.tools)
  const runtimeNotes = useAgentStore((state) => state.runtimeNotes)
  const notifications = useAgentStore((state) => state.notifications)
  const finished = useAgentStore((state) => state.finished)
  const stopReason = useAgentStore((state) => state.stopReason)
  const usage = useAgentStore((state) => state.usage)
  const contextWindow = useAgentStore((state) => state.contextWindow)
  const timing = useAgentStore((state) => state.timing)
  const round = useAgentStore((state) => state.round)
  const retries = useAgentStore((state) => state.retries)
  const messageCount = useAgentStore((state) => state.messageCount)

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2" data-testid="agent-transcript">
      <ThinkingSection text={thinkingText} />

      {assistantText !== '' ? (
        <pre className="text-[12px] leading-relaxed whitespace-pre-wrap text-fg">
          {assistantText}
        </pre>
      ) : (
        thinkingText === '' && tools.length === 0 && (
          <p className="text-[11px] text-faint">No agent activity yet. Send a prompt below.</p>
        )
      )}

      {tools.length > 0 && (
        <ul className="mt-2 space-y-0.5" data-testid="agent-tools">
          {tools.map((tool) => (
            <ToolRow key={tool.key} tool={tool} />
          ))}
        </ul>
      )}

      {runtimeNotes.length > 0 && (
        <div className="mt-2 space-y-0.5">
          {runtimeNotes.map((note, index) => (
            <p key={index} className="font-mono text-[10px] text-faint">
              · {note}
            </p>
          ))}
        </div>
      )}

      <SessionMeta
        usage={usage}
        contextWindow={contextWindow}
        timing={timing}
        round={round}
        retries={retries}
        messageCount={messageCount}
      />

      {stopReason !== null && (
        <p className="mt-2 rounded-sm border border-warn/40 bg-warn/10 px-2 py-1 text-[11px] text-warn">
          Stopped: {stopReason}
        </p>
      )}
      {finished !== null && (
        finished.ok ? (
          <p className="mt-2 rounded-sm border border-ok/40 bg-ok/10 px-2 py-1 text-[11px] text-ok">
            Finished
          </p>
        ) : (
          <p className="mt-2 rounded-sm border border-danger/40 bg-danger/10 px-2 py-1 text-[11px] whitespace-pre-wrap text-danger">
            Failed{finished.error !== null ? `: ${finished.error}` : ''}
          </p>
        )
      )}

      {notifications.length > 0 && (
        <div className="mt-3 border-t border-border pt-2">
          {notifications.map((notification) => (
            <p key={notification.id} className="py-0.5 text-[11px] text-warn">
              ⚑ {notification.message}
            </p>
          ))}
        </div>
      )}
    </div>
  )
}

function ThinkingSection({ text }: { text: string }) {
  if (text === '') return null
  return (
    <Collapsible className="mb-2" data-testid="agent-thinking">
      <CollapsibleTrigger className="group flex items-center gap-1 text-[10px] font-semibold tracking-wider text-faint uppercase hover:text-muted">
        <ChevronRight size={10} className="transition-transform group-data-[state=open]:rotate-90" />
        Thinking
      </CollapsibleTrigger>
      <CollapsibleContent>
        <pre className="mt-1 max-h-48 overflow-y-auto rounded-sm border border-border bg-panel px-2 py-1.5 text-[11px] whitespace-pre-wrap text-muted">
          {text}
        </pre>
      </CollapsibleContent>
    </Collapsible>
  )
}

function ToolRow({ tool }: { tool: import('@/types/agent').ToolRun }) {
  const [expanded, setExpanded] = useState(false)
  const icon =
    tool.status === 'finished' ? (
      <CircleCheck size={11} className="shrink-0 text-ok" />
    ) : tool.status === 'failed' ? (
      <CircleX size={11} className="shrink-0 text-danger" />
    ) : tool.status === 'running' ? (
      <Loader2 size={11} className="shrink-0 animate-spin text-accent" />
    ) : (
      <CircleDashed size={11} className="shrink-0 text-faint" />
    )
  return (
    <li>
      <button
        type="button"
        className="flex w-full items-center gap-1.5 rounded-sm px-1 py-0.5 text-left transition-colors hover:bg-raised"
        onClick={() => tool.detail !== null && setExpanded((value) => !value)}
      >
        {icon}
        <span className="rounded-sm border border-border px-1 font-mono text-[9px] text-muted uppercase">
          {tool.kind}
        </span>
        <span className="truncate text-[11px] text-fg" title={tool.label}>
          {tool.label}
        </span>
      </button>
      {expanded && tool.detail !== null && (
        <pre className="mx-4 mt-0.5 mb-1 max-h-40 overflow-y-auto rounded-sm border border-border bg-bg px-2 py-1 font-mono text-[10px] whitespace-pre-wrap text-muted">
          {tool.detail}
        </pre>
      )}
    </li>
  )
}

function SessionMeta({
  usage,
  contextWindow,
  timing,
  round,
  retries,
  messageCount,
}: {
  usage: import('@/types/agent').TokenUsage | null
  contextWindow: import('@/types/agent').ContextWindowInfo | null
  timing: import('@/types/agent').ResponseTimingInfo | null
  round: number | null
  retries: number
  messageCount: number | null
}) {
  if (usage === null && contextWindow === null && timing === null && round === null && retries === 0 && messageCount === null) {
    return null
  }
  const parts: string[] = []
  if (usage !== null) {
    parts.push(`in ${fmtTokens(usage.input_tokens + usage.cache_read_input_tokens)}`)
    parts.push(`out ${fmtTokens(usage.output_tokens)}`)
    if (usage.cache_creation_input_tokens > 0) {
      parts.push(`cache-w ${fmtTokens(usage.cache_creation_input_tokens)}`)
    }
  }
  if (contextWindow !== null) {
    const percent = contextWindow.capacity > 0 ? Math.round((contextWindow.tokens / contextWindow.capacity) * 100) : 0
    parts.push(`ctx ${percent}%`)
  }
  if (timing !== null) {
    parts.push(`${(timing.elapsed_ms / 1000).toFixed(1)}s`)
  }
  if (round !== null) parts.push(`round ${round}`)
  if (retries > 0) parts.push(`retries ${retries}`)
  if (messageCount !== null) parts.push(`${messageCount} msg(s)`)
  return <p className="mt-2 font-mono text-[10px] text-faint">{parts.join(' · ')}</p>
}

function fmtTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`
  return String(value)
}
