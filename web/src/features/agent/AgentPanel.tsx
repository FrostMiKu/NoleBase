import { useEffect, useRef, useState } from 'react'
import { ArrowDown, RadioTower, SendHorizontal, Square } from 'lucide-react'
import { toast } from 'sonner'

import { clearSubmittedDraft, useDraft } from '@/hooks/use-draft'
import { acknowledgeAgentRun } from './agent-reducer'

import { buttonStyles } from '@/components/ui/dialog'
import { agentApi } from '@/api/agent'
import { useAgentStore } from '@/stores/agent-store'
import { useUiStore } from '@/stores/ui-store'
import { AgentInteractions } from './AgentInteractions'
import { AgentTranscript } from './AgentTranscript'
import { PermissionSelect } from './PermissionSelect'

function ConnectionDot() {
  const connection = useAgentStore((state) => state.connection)
  const color =
    connection === 'connected' ? 'bg-ok' : connection === 'connecting' ? 'bg-warn' : 'bg-danger'
  return (
    <span
      className={`inline-block h-1.5 w-1.5 rounded-full ${color}`}
      title={`Agent WS: ${connection}`}
    />
  )
}

/**
 * Right-sidebar agent surface: header (connection, permission, stop),
 * live transcript, pending interactions, and the prompt composer.
 * Enter sends; Shift+Enter inserts a newline.
 */
export function AgentPanel() {
  const running = useAgentStore((state) => state.running)
  const streamInterrupted = useAgentStore((state) => state.streamInterrupted)
  const dismissInterruption = useAgentStore((state) => state.dismissInterruption)
  const agentFocusTick = useUiStore((state) => state.agentFocusTick)
  const [prompt, setPrompt] = useDraft('agent')
  const [following, setFollowing] = useState(true)
  const followingRef = useRef(true)
  const connection = useAgentStore((state) => state.connection)
  const sending = useAgentStore((state) => state.runSubmitting)
  const composerRef = useRef<HTMLTextAreaElement | null>(null)
  const transcriptScrollRef = useRef<HTMLDivElement | null>(null)
  const lastTurn = useAgentStore((state) => state.turns[state.turns.length - 1])

  // "Focus Agent" command.
  useEffect(() => {
    if (agentFocusTick === 0) return
    composerRef.current?.focus()
  }, [agentFocusTick])

  // Keep the newest output visible while streaming.
  useEffect(() => {
    const container = transcriptScrollRef.current
    if (container !== null && followingRef.current) container.scrollTop = container.scrollHeight
  }, [lastTurn])

  const send = () => {
    const trimmed = prompt.trim()
    if (trimmed === '' || running || sending || connection !== 'connected') return
    const previousSeq = useAgentStore.getState().turnSeq
    followingRef.current = true
    setFollowing(true)
    useAgentStore.setState({ runSubmitting: true })
    agentApi
      .run(trimmed)
      .then(() => {
        // 202: the agent took the task; output arrives over the WS as a
        // new transcript turn — previous turns stay visible.
        useAgentStore.setState((state) => acknowledgeAgentRun(state, trimmed, previousSeq))
        clearSubmittedDraft('agent', prompt)
      })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : 'the agent command failed'
        toast.error(`Run failed: ${message}`)
      })
      .finally(() => useAgentStore.setState({ runSubmitting: false }))
  }

  const cancel = () => {
    // Do not assume the run finished; wait for stopped/finished events.
    agentApi.cancel().catch((error: unknown) => {
      toast.error(`Cancel failed: ${error instanceof Error ? error.message : 'unknown error'}`)
    })
  }

  return (
    <section className="flex h-full flex-col" data-testid="agent-panel">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-panel px-3">
        <ConnectionDot />
        <span className="flex items-center gap-1 text-[10px] font-semibold tracking-wider text-faint uppercase">
          <RadioTower size={11} />
          Agent
        </span>
        <div className="ml-auto flex items-center gap-1.5">
          <PermissionSelect />
          {running && (
            <button
              type="button"
              className={`${buttonStyles.danger} h-6 px-2`}
              onClick={cancel}
              title="Stop the running agent"
            >
              <Square size={10} className="mr-1 inline" />
              Stop
            </button>
          )}
        </div>
      </div>

      {streamInterrupted && (
        <div className="flex shrink-0 items-center gap-2 border-b border-warn/40 bg-warn/10 px-3 py-1 text-[11px] text-warn">
          <span>Agent stream was interrupted. Some output may be missing.</span>
          <button
            type="button"
            className="ml-auto text-muted hover:text-fg"
            onClick={dismissInterruption}
          >
            Dismiss
          </button>
        </div>
      )}

      <div
        ref={transcriptScrollRef}
        onScroll={() => {
          const el = transcriptScrollRef.current
          if (!el) return
          followingRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48
          setFollowing(followingRef.current)
        }}
        className="flex min-h-0 flex-1 flex-col overflow-y-auto"
      >
        <AgentTranscript />
      </div>

      {!following && (
        <button
          type="button"
          className="mx-auto my-1 flex items-center gap-1 rounded border border-border px-2 py-1 text-xs text-accent"
          onClick={() => {
            const el = transcriptScrollRef.current
            if (el) el.scrollTop = el.scrollHeight
            followingRef.current = true
            setFollowing(true)
          }}
        >
          <ArrowDown size={12} /> Latest output
        </button>
      )}
      <AgentInteractions />

      <div className="shrink-0 border-t border-border bg-panel p-2">
        <textarea
          ref={composerRef}
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          onKeyDown={(event) => {
            // IME composition confirms candidates with Enter; never send.
            if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault()
              send()
            }
          }}
          aria-label="Agent prompt"
          disabled={sending}
          placeholder={running ? 'Draft your next question…' : 'Ask the agent…'}
          data-testid="agent-composer"
          className="max-h-48 w-full resize-none overflow-y-auto field-sizing-content rounded-sm border border-border-strong bg-bg px-2 py-1.5 text-[12px] text-fg outline-none placeholder:text-faint focus:border-accent disabled:opacity-60"
        />
        <div className="mt-1 flex items-center justify-between gap-2">
          <span className="text-[10px] text-faint">
            {connection !== 'connected'
              ? 'Reconnecting to Agent…'
              : running
                ? 'Agent is working'
                : 'Enter to send · Shift+Enter for newline'}
          </span>
          <button
            type="button"
            className={buttonStyles.primary}
            onClick={send}
            disabled={sending || running || !prompt.trim() || connection !== 'connected'}
          >
            <SendHorizontal size={12} className="mr-1" />
            {sending ? 'Sending…' : 'Send'}
          </button>
        </div>
      </div>
    </section>
  )
}
