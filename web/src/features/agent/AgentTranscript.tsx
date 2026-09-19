import { useState } from 'react'
import { ChevronRight, CircleCheck, CircleDashed, CircleX, Loader2 } from 'lucide-react'

import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { Markdown } from '@/features/agent/Markdown'
import { useAgentStore } from '@/stores/agent-store'
import type { AgentTurn } from '@/features/agent/agent-reducer'
import type { ToolRun } from '@/types/agent'

/** Collapse 3+ blank lines to one blank line and trim the ends: model
 * output sometimes carries runs of newlines that read as huge gaps in a
 * plain-text projection. Display-only; the store keeps the raw text. */
function displayText(text: string): string {
  return text.replace(/\n{3,}/g, '\n\n').trim()
}

/**
 * The live runtime view: one compact block per conversational turn (prompt
 * echo, collapsed-by-default thinking, assistant text, tool/job timeline,
 * runtime notices, meta line with the outcome chip). Turns accumulate.
 */
export function AgentTranscript() {
  const turns = useAgentStore((state) => state.turns)
  const contextWindow = useAgentStore((state) => state.contextWindow)
  const messageCount = useAgentStore((state) => state.messageCount)
  const notifications = useAgentStore((state) => state.notifications)

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2" data-testid="agent-transcript">
      {turns.length === 0 && notifications.length === 0 && (
        <p className="text-[11px] text-faint">No agent activity yet. Send a prompt below.</p>
      )}

      {turns.map((turn) => (
        <TurnBlock key={turn.id} turn={turn} />
      ))}

      {(contextWindow !== null || messageCount !== null) && (
        <p className="mt-1.5 font-mono text-[10px] text-faint">
          {contextWindow !== null &&
            `ctx ${contextWindow.capacity > 0 ? Math.round((contextWindow.tokens / contextWindow.capacity) * 100) : 0}%`}
          {contextWindow !== null && messageCount !== null && ' · '}
          {messageCount !== null && `${messageCount} msg(s) in session`}
        </p>
      )}

      {notifications.length > 0 && (
        <div className="mt-2 border-t border-border pt-1.5">
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

function TurnBlock({ turn }: { turn: AgentTurn }) {
  const outcome =
    turn.finished !== null
      ? turn.finished.ok
        ? { tone: 'text-ok', label: '✓ Finished' }
        : {
            tone: 'text-danger',
            label:
              turn.finished.error !== null
                ? `✗ Failed: ${turn.finished.error}`
                : '✗ Failed',
          }
      : turn.stopReason !== null
        ? { tone: 'text-warn', label: `■ Stopped: ${turn.stopReason}` }
        : null

  return (
    <div className="mb-2 border-b border-border pb-1.5 last:border-b-0" data-testid={`agent-turn-${turn.id}`}>
      {turn.prompt !== null && (
        <p className="flex items-start gap-1.5 text-[11px] text-muted">
          <span className="mt-px shrink-0 text-faint">›</span>
          <span className="whitespace-pre-wrap">{turn.prompt}</span>
        </p>
      )}

      <ThinkingSection text={turn.thinkingText} />

      {turn.assistantText !== '' && <Markdown text={displayText(turn.assistantText)} />}

      {turn.tools.length > 0 && (
        <ul className="mt-1 space-y-px" data-testid="agent-tools">
          {turn.tools.map((tool) => (
            <ToolRow key={tool.key} tool={tool} />
          ))}
        </ul>
      )}

      {turn.runtimeNotes.length > 0 && (
        <div className="mt-1 space-y-px">
          {turn.runtimeNotes.map((note, index) => (
            <p key={index} className="font-mono text-[10px] text-faint">
              · {note}
            </p>
          ))}
        </div>
      )}

      <TurnMeta turn={turn} outcome={outcome} />
    </div>
  )
}

function ThinkingSection({ text }: { text: string }) {
  if (text === '') return null
  return (
    <Collapsible className="my-1" data-testid="agent-thinking">
      <CollapsibleTrigger className="group flex items-center gap-1 text-[10px] font-semibold tracking-wider text-faint uppercase hover:text-muted">
        <ChevronRight size={10} className="transition-transform group-data-[state=open]:rotate-90" />
        Thinking
      </CollapsibleTrigger>
      <CollapsibleContent>
        <pre className="mt-1 max-h-48 overflow-y-auto rounded-sm border border-border bg-panel px-2 py-1 text-[11px] whitespace-pre-wrap text-muted">
          {displayText(text)}
        </pre>
      </CollapsibleContent>
    </Collapsible>
  )
}

function ToolRow({ tool }: { tool: ToolRun }) {
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

function TurnMeta({
  turn,
  outcome,
}: {
  turn: AgentTurn
  outcome: { tone: string; label: string } | null
}) {
  const parts: string[] = []
  if (turn.usage !== null) {
    parts.push(`in ${fmtTokens(turn.usage.input_tokens + turn.usage.cache_read_input_tokens)}`)
    parts.push(`out ${fmtTokens(turn.usage.output_tokens)}`)
    if (turn.usage.cache_creation_input_tokens > 0) {
      parts.push(`cache-w ${fmtTokens(turn.usage.cache_creation_input_tokens)}`)
    }
  }
  if (turn.timing !== null) parts.push(`${(turn.timing.elapsed_ms / 1000).toFixed(1)}s`)
  if (turn.round !== null) parts.push(`round ${turn.round}`)
  if (turn.retries > 0) parts.push(`retries ${turn.retries}`)

  if (parts.length === 0 && outcome === null) return null
  return (
    <p className="mt-1 font-mono text-[10px] text-faint">
      {parts.join(' · ')}
      {parts.length > 0 && outcome !== null && ' · '}
      {outcome !== null && <span className={outcome.tone}>{outcome.label}</span>}
    </p>
  )
}

function fmtTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`
  return String(value)
}
